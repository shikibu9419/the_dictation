//! End-to-end IPC tests: synthetic ring TLV -> Index adapter -> mock ASR.
//! No Bluetooth helper, microphone, GUI, permissions or model assets are used.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct Worker {
    child: Child,
    input: Option<ChildStdin>,
    output: Lines<BufReader<ChildStdout>>,
    sequence: u64,
    events: Vec<Value>,
}
impl Worker {
    async fn new(dir: &Path, live: bool) -> Self {
        Self::with_engine(dir, live, r#"#!/bin/sh
printf '{"type":"ready"}\n'
while IFS= read -r line; do
printf '%s\t%s\n' "$2" "$line" >> "$ENGINE_LOG"
case "$line" in
*'"type":"audio"'*) printf '{"type":"accepted"}\n'; if [ "$2" = live ]; then printf '{"type":"partial","text":"live words"}\n'; fi;;
*'"type":"finish"'*) printf '{"type":"final","text":"whole recording"}\n';;
*'"type":"cancel"'*) printf '{"type":"cancelled"}\n';;
esac
done
"#).await
    }
    async fn with_engine(dir: &Path, live: bool, script: &str) -> Self {
        let engine = dir.join("mock-asr");
        std::fs::write(&engine, script).unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = dir.join("pebble-index-rust");
        std::fs::create_dir(&config).unwrap();
        std::fs::write(
            config.join("settings.json"),
            json!({"presentation":{"live_mode":live}}).to_string(),
        )
        .unwrap();
        let options = json!({"address":"synthetic-ring", "language":"ja_JP", "command":"listen", "verbose":false});
        let mut child = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
            .arg("__worker")
            .arg(options.to_string())
            .env("XDG_CONFIG_HOME", dir)
            .env("INDEX_VOICE_SPEECH_COMMAND", engine)
            .env("ENGINE_LOG", dir.join("engine.log"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(dir.join("worker.log")).unwrap())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut worker = Self {
            child,
            input,
            output,
            sequence: 0,
            events: vec![],
        };
        worker.until(|v| v["type"] == "ready").await;
        worker.send(0, json!({"type":"boundary","index":1})).await;
        worker
    }
    async fn send(&mut self, time: u64, mut message: Value) {
        self.sequence += 1;
        message["received_ms"] = json!(time);
        message["received_seq"] = json!(self.sequence);
        let line = format!("{message}\n");
        self.input
            .as_mut()
            .unwrap()
            .write_all(line.as_bytes())
            .await
            .unwrap();
    }
    async fn until(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let line = self
                    .output
                    .next_line()
                    .await
                    .unwrap()
                    .expect("worker exited before result");
                let event: Value = serde_json::from_str(&line).unwrap();
                assert_ne!(event["type"], "error", "{event}");
                self.events.push(event.clone());
                if predicate(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("worker IPC deadline")
    }
    async fn close(mut self) {
        drop(self.input.take());
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
    }
}
fn collection(
    index: u16,
    start: u32,
    final_part: bool,
    samples: usize,
    pattern: u32,
    count: u32,
) -> Value {
    let mut records = vec![80];
    records.extend((4 + samples as u32 * 2).to_le_bytes());
    records.extend(1000u32.to_le_bytes());
    for i in 0..samples {
        records.extend((if i % 2 == 0 { 1000i16 } else { -1000 }).to_le_bytes());
    }
    records.push(82);
    records.extend(6u16.to_le_bytes());
    records.extend(start.to_le_bytes());
    records.extend([1, final_part as u8]);
    records.push(83);
    records.extend(8u16.to_le_bytes());
    records.extend(pattern.to_le_bytes());
    records.extend(count.to_le_bytes());
    let mut raw = ((records.len() + 4) as u32).to_le_bytes().to_vec();
    raw.extend(records);
    json!({"type":"collection","index":index,"raw":STANDARD.encode(raw)})
}
fn batch_sizes(dir: &Path) -> Vec<usize> {
    let mut sizes = vec![];
    let mut current = 0;
    for line in std::fs::read_to_string(dir.join("engine.log"))
        .unwrap_or_default()
        .lines()
    {
        let Some(raw) = line.strip_prefix("batch\t") else {
            continue;
        };
        let value: Value = serde_json::from_str(raw).unwrap();
        if value["type"] == "audio" {
            current += STANDARD
                .decode(value["pcm"].as_str().unwrap())
                .unwrap()
                .len()
                / 2;
        }
        if value["type"] == "finish" {
            sizes.push(current);
            current = 0;
        }
    }
    sizes
}

#[tokio::test]
async fn counter_reset_preserves_pending_whole_audio_and_ignores_its_old_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let script = r#"#!/bin/sh
printf '{"type":"ready"}\n'
while IFS= read -r line; do
printf '%s\t%s\n' "$2" "$line" >> "$ENGINE_LOG"
case "$line" in
*'"type":"audio"'*) printf '{"type":"accepted"}\n';;
*'"type":"finish"'*)
while [ ! -e "$ENGINE_LOG.release" ]; do sleep 0.01; done
printf '{"type":"final","text":"whole recording"}\n';;
*'"type":"cancel"'*) printf '{"type":"cancelled"}\n';;
esac
done
"#;
    let mut w = Worker::with_engine(dir.path(), false, script).await;
    w.send(1, json!({"type":"range","start":1,"end":4})).await;
    w.send(2, collection(1, 1, false, 100, 0, 0)).await;
    w.send(3, collection(2, 1, false, 100, 0, 0)).await;
    w.send(4, collection(3, 1, true, 23, 1, 1)).await;
    w.send(54, json!({"type":"clock"})).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while batch_sizes(dir.path()).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    w.send(60, json!({"type":"range","start":1,"end":2})).await;
    w.send(61, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(111, json!({"type":"clock"})).await;
    w.until(|v| v["type"] == "reception_activity").await;
    w.send(120, collection(1, 1, true, 17, 1, 1)).await;
    w.send(170, json!({"type":"clock"})).await;
    std::fs::write(dir.path().join("engine.log.release"), []).unwrap();
    w.send(171, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    let finals: Vec<_> = w
        .events
        .iter()
        .filter(|v| v["type"] == "text" && v["final"] == true)
        .collect();
    assert_eq!(finals.len(), 2);
    assert_ne!(finals[0]["recording"], finals[1]["recording"]);
    assert_eq!(batch_sizes(dir.path()), [223, 17]);
    let cursor = std::fs::read_dir(dir.path().join("pebble-index-rust"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("cursor-")
        })
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(cursor).unwrap()).unwrap()["next_index"],
        2
    );
    w.close().await;
}

#[tokio::test]
async fn evicted_prefix_and_late_final_do_not_stop_a_following_complete_recording() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), false).await;
    w.send(1, json!({"type":"range","start":3,"end":5})).await;
    w.send(2, collection(3, 1, false, 100, 0, 0)).await;
    let lost = w.until(|v| v["type"] == "discarded").await;
    w.send(3, collection(4, 1, true, 23, 1, 1)).await;
    w.send(4, json!({"type":"range","start":3,"end":6})).await;
    w.send(5, json!({"type":"button_state","pressed":true,"unread":5}))
        .await;
    w.send(6, collection(5, 5, true, 17, 3, 2)).await;
    w.send(56, json!({"type":"clock"})).await;
    let complete = w.until(|v| v["type"] == "text" && v["final"] == true).await;
    assert_ne!(lost["recording"], complete["recording"]);
    assert_eq!(batch_sizes(dir.path()), [17]);
    assert!(!w.events.iter().any(|v| v["type"] == "gesture"));
    // The lost source is still an error at EOF, never a fabricated success.
    drop(w.input.take());
    let mut reported_loss = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = w.output.next_line().await.unwrap() {
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_ne!(value["type"], "flushed");
            reported_loss |= value["type"] == "error";
        }
        assert!(!w.child.wait().await.unwrap().success());
    })
    .await
    .unwrap();
    assert!(reported_loss);
}

#[tokio::test]
async fn eof_drains_complete_audio_persists_its_checkpoint_and_closes_both_engines() {
    let dir = tempfile::tempdir().unwrap();
    let mut script = failing_engine("never", "never").replace(
        "printf '{\"type\":\"final\"",
        "sleep 0.1; printf '{\"type\":\"final\"",
    );
    script.push_str("printf '%s\\t{\"type\":\"test_eof\"}\\n' \"$2\" >> \"$ENGINE_LOG\"\n");
    let mut w = Worker::with_engine(dir.path(), true, &script).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(10, collection(1, 1, false, 400, 0, 0)).await;
    w.send(60, json!({"type":"clock"})).await;
    w.send(
        80,
        json!({"type":"button_state","pressed":false,"unread":2}),
    )
    .await;
    w.send(90, collection(2, 1, true, 100, 1, 1)).await;
    w.send(140, json!({"type":"clock"})).await;
    drop(w.input.take()); // No explicit flush and no waiting for ASR first.
    w.until(|v| v["type"] == "flushed").await;
    assert_eq!(
        w.events
            .iter()
            .filter(|v| v["type"] == "text" && v["final"] == true)
            .count(),
        1
    );
    assert_eq!(batch_sizes(dir.path()), [500]);
    let cursor = std::fs::read_dir(dir.path().join("pebble-index-rust"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("cursor-")
        })
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(cursor).unwrap()).unwrap()["next_index"],
        3
    );
    w.close().await;
    let log = std::fs::read_to_string(dir.path().join("engine.log")).unwrap();
    for mode in ["live", "batch"] {
        assert!(
            log.contains(&format!("{mode}\t{{\"type\":\"test_eof\"}}")),
            "{log}"
        );
    }
}

#[tokio::test]
async fn eof_with_missing_ring_final_reports_failure_instead_of_successful_completion() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), true).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(10, collection(1, 1, false, 400, 0, 0)).await;
    drop(w.input.take());
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut failed = false;
        while let Some(line) = w.output.next_line().await.unwrap() {
            let value: Value = serde_json::from_str(&line).unwrap();
            assert_ne!(value["final"], true);
            assert_ne!(value["type"], "flushed");
            if value["type"] == "error" {
                assert!(
                    value["text"]
                        .as_str()
                        .unwrap()
                        .contains("before final audio")
                );
                failed = true;
            }
        }
        assert!(failed);
        assert!(!w.child.wait().await.unwrap().success());
    })
    .await
    .unwrap();
}

fn failing_engine(mode: &str, point: &str) -> String {
    format!(
        r#"#!/bin/sh
mode="$2"
count=0
printf '%s\t{{"type":"test_start","pid":%s}}\n' "$mode" "$$" >> "$ENGINE_LOG"
printf '{{"type":"ready"}}\n'
while IFS= read -r line; do
printf '%s\t%s\n' "$mode" "$line" >> "$ENGINE_LOG"
case "$line" in
*'"type":"audio"'*)
  count=$((count + 1))
  if [ "$mode" = '{mode}' ] && [ '{point}' = before_ack ] && [ "$count" = 2 ] && mkdir "$ENGINE_LOG.failed" 2>/dev/null; then exit 9; fi
  printf '{{"type":"accepted"}}\n'
  if [ "$mode" = live ]; then printf '{{"type":"partial","text":"live words"}}\n'; fi
  if [ "$mode" = '{mode}' ] && [ '{point}' = after_ack ] && mkdir "$ENGINE_LOG.failed" 2>/dev/null; then exit 9; fi;;
*'"type":"finish"'*)
  if [ "$mode" = '{mode}' ] && [ '{point}' = finish ] && mkdir "$ENGINE_LOG.failed" 2>/dev/null; then exit 9; fi
  printf '{{"type":"final","text":"whole recording"}}\n'
  if [ "$mode" = '{mode}' ] && [ '{point}' = after_final ] && mkdir "$ENGINE_LOG.failed" 2>/dev/null; then exit 9; fi;;
*'"type":"cancel"'*) printf '{{"type":"cancelled"}}\n';;
esac
done
"#
    )
}

fn attempts(dir: &Path, mode: &str) -> Vec<Vec<Value>> {
    let mut attempts: Vec<Vec<Value>> = vec![];
    for line in std::fs::read_to_string(dir.join("engine.log"))
        .unwrap()
        .lines()
    {
        let Some((role, value)) = line.split_once('\t') else {
            continue;
        };
        if role != mode {
            continue;
        }
        let value: Value = serde_json::from_str(value).unwrap();
        if value["type"] == "test_start" {
            attempts.push(vec![]);
        } else {
            attempts.last_mut().unwrap().push(value);
        }
    }
    attempts
}
fn pcm_of(commands: &[Value]) -> Vec<u8> {
    commands
        .iter()
        .filter(|v| v["type"] == "audio")
        .flat_map(|v| STANDARD.decode(v["pcm"].as_str().unwrap()).unwrap())
        .collect()
}

#[tokio::test]
async fn eof_after_final_does_not_replay_or_emit_the_completed_recording_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut w =
        Worker::with_engine(dir.path(), false, &failing_engine("batch", "after_final")).await;
    w.send(10, collection(1, 1, true, 351, 1, 1)).await;
    w.send(60, json!({"type":"clock"})).await;
    w.until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while attempts(dir.path(), "batch").len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    w.send(70, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert!(pcm_of(&attempts(dir.path(), "batch")[1]).is_empty());
    assert_eq!(
        w.events
            .iter()
            .filter(|v| v["type"] == "text" && v["mode"] == "batch")
            .count(),
        1
    );
    w.close().await;
}

#[tokio::test]
async fn a_failed_restart_attempt_keeps_the_pending_full_recording() {
    let dir = tempfile::tempdir().unwrap();
    let script = failing_engine("batch", "finish").replace(
        "printf '{\"type\":\"ready\"}\\n'",
        "if [ -d \"$ENGINE_LOG.failed\" ] && mkdir \"$ENGINE_LOG.boot-failed\" 2>/dev/null; then printf '{\"type\":\"error\",\"text\":\"synthetic startup failure\"}\\n'; exit 8; fi\nprintf '{\"type\":\"ready\"}\\n'",
    );
    assert!(script.contains("synthetic startup failure"));
    let mut w = Worker::with_engine(dir.path(), false, &script).await;
    w.send(10, collection(1, 1, true, 351, 1, 1)).await;
    w.send(60, json!({"type":"clock"})).await;
    let text = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_eq!(text["audio_seconds"], 0.351);
    w.send(100, collection(2, 2, true, 4, 1, 2)).await;
    w.send(150, json!({"type":"clock"})).await;
    assert_eq!(
        w.until(|v| v["type"] == "gesture").await["gesture"],
        "single_tap"
    );
    w.send(160, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    let batch = attempts(dir.path(), "batch");
    assert_eq!(batch.len(), 3);
    assert_eq!(pcm_of(&batch[0]), pcm_of(&batch[2]));
    assert!(pcm_of(&batch[1]).is_empty());
    assert_eq!(
        w.events
            .iter()
            .filter(|v| v["type"] == "text" && v["mode"] == "batch")
            .count(),
        1
    );
    w.close().await;
}

#[tokio::test]
async fn release_during_model_restart_skips_old_live_replay_but_preserves_the_full_batch() {
    let dir = tempfile::tempdir().unwrap();
    let script = failing_engine("live", "before_ack").replace(
        "printf '{\"type\":\"ready\"}\\n'",
        "if [ \"$mode\" = live ] && [ -d \"$ENGINE_LOG.failed\" ]; then sleep 0.2; fi\nprintf '{\"type\":\"ready\"}\\n'",
    );
    assert!(script.contains("sleep 0.2"));
    let mut w = Worker::with_engine(dir.path(), true, &script).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(51, json!({"type":"clock"})).await;
    w.send(60, collection(1, 1, false, 250, 0, 0)).await;
    w.until(|v| v["type"] == "text" && v["mode"] == "live")
        .await;
    w.send(80, collection(2, 1, false, 250, 0, 0)).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while attempts(dir.path(), "live").len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    w.send(
        100,
        json!({"type":"button_state","pressed":false,"unread":3}),
    )
    .await;
    w.send(110, collection(3, 1, true, 100, 1, 1)).await;
    w.send(160, json!({"type":"clock"})).await;
    let text = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_eq!(text["audio_seconds"], 0.6);
    w.send(170, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert!(pcm_of(&attempts(dir.path(), "live")[1]).is_empty());
    assert_eq!(
        w.events
            .iter()
            .filter(|v| v["type"] == "text" && v["mode"] == "live")
            .count(),
        1
    );
    assert_eq!(attempts(dir.path(), "batch").len(), 1);
    w.close().await;
}

#[tokio::test]
async fn live_worker_crash_replays_accepted_and_unacknowledged_pcm_without_restarting_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::with_engine(dir.path(), true, &failing_engine("live", "before_ack")).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(51, json!({"type":"clock"})).await;
    w.send(60, collection(1, 1, false, 250, 0, 0)).await;
    w.until(|v| v["type"] == "text" && v["mode"] == "live")
        .await;
    w.send(80, collection(2, 1, false, 250, 0, 0)).await;
    // No new input is needed to replay both chunks with fresh filter state.
    for _ in 0..2 {
        w.until(|v| v["type"] == "text" && v["mode"] == "live")
            .await;
    }
    let live = attempts(dir.path(), "live");
    assert_eq!(live.len(), 2);
    assert_eq!(pcm_of(&live[0]), pcm_of(&live[1]));
    assert_eq!(pcm_of(&live[1]).len(), 1_000);
    assert_eq!(attempts(dir.path(), "batch").len(), 1);
    w.send(
        100,
        json!({"type":"button_state","pressed":false,"unread":3}),
    )
    .await;
    w.send(110, collection(3, 1, true, 100, 1, 1)).await;
    w.send(160, json!({"type":"clock"})).await;
    let text = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_eq!(text["audio_seconds"], 0.6);
    w.send(170, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert_eq!(attempts(dir.path(), "batch").len(), 1);
    w.close().await;
}

#[tokio::test]
async fn eof_after_accepted_is_recovered_without_a_new_audio_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::with_engine(dir.path(), true, &failing_engine("live", "after_ack")).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(51, json!({"type":"clock"})).await;
    w.send(60, collection(1, 1, false, 250, 0, 0)).await;
    for _ in 0..2 {
        w.until(|v| v["type"] == "text" && v["mode"] == "live")
            .await;
    }
    let live = attempts(dir.path(), "live");
    assert_eq!(live.len(), 2);
    assert_eq!(pcm_of(&live[0]), pcm_of(&live[1]));
    w.send(
        100,
        json!({"type":"button_state","pressed":false,"unread":2}),
    )
    .await;
    w.send(110, collection(2, 1, true, 23, 1, 1)).await;
    w.send(160, json!({"type":"clock"})).await;
    let text = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_eq!(text["audio_seconds"], 0.273);
    w.send(170, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    w.close().await;
}

#[tokio::test]
async fn batch_finish_crash_replays_the_whole_recording_and_keeps_live_and_the_next_recording() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::with_engine(dir.path(), true, &failing_engine("batch", "finish")).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(51, json!({"type":"clock"})).await;
    w.send(60, collection(1, 1, false, 250, 0, 0)).await;
    w.until(|v| v["type"] == "text" && v["mode"] == "live")
        .await;
    w.send(
        100,
        json!({"type":"button_state","pressed":false,"unread":2}),
    )
    .await;
    w.send(110, collection(2, 1, true, 101, 1, 1)).await;
    w.send(160, json!({"type":"clock"})).await;
    // Reception of the next recording is independent of the failed batch.
    w.send(
        200,
        json!({"type":"button_state","pressed":true,"unread":3}),
    )
    .await;
    w.send(250, json!({"type":"clock"})).await;
    w.send(260, collection(3, 3, false, 250, 1, 1)).await;
    w.send(
        300,
        json!({"type":"button_state","pressed":false,"unread":4}),
    )
    .await;
    w.send(310, collection(4, 3, true, 27, 3, 2)).await;
    w.send(360, json!({"type":"clock"})).await;
    let first = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    let second = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_eq!(first["audio_seconds"], 0.351);
    assert_eq!(second["audio_seconds"], 0.277);
    assert_ne!(first["recording"], second["recording"]);
    w.send(400, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    let batch = attempts(dir.path(), "batch");
    assert_eq!(batch.len(), 2);
    let first_pcm = pcm_of(&batch[0]);
    assert_eq!(first_pcm.len(), 702);
    assert_eq!(&pcm_of(&batch[1])[..702], first_pcm);
    assert_eq!(pcm_of(&batch[1]).len(), (351 + 277) * 2);
    assert_eq!(attempts(dir.path(), "live").len(), 1);
    assert_eq!(
        w.events
            .iter()
            .filter(|v| v["type"] == "text" && v["mode"] == "batch")
            .count(),
        2
    );
    w.close().await;
}

#[tokio::test]
async fn ring_audio_uses_live_then_complete_batch_and_supports_a_second_recording() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), true).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(51, json!({"type":"clock"})).await;
    w.send(60, collection(1, 1, false, 250, 0, 0)).await;
    w.until(|v| v["type"] == "text" && v["mode"] == "live")
        .await;
    w.send(
        100,
        json!({"type":"button_state","pressed":false,"unread":2}),
    )
    .await;
    w.send(150, json!({"type":"clock"})).await;
    w.send(170, collection(2, 1, true, 49, 1, 1)).await;
    let first = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_eq!(first["audio_seconds"], 0.299);
    w.send(
        200,
        json!({"type":"button_state","pressed":true,"unread":3}),
    )
    .await;
    w.send(250, json!({"type":"clock"})).await;
    w.send(260, collection(3, 3, false, 200, 1, 1)).await;
    w.until(|v| v["type"] == "text" && v["mode"] == "live")
        .await;
    w.send(300, collection(4, 3, true, 19, 3, 2)).await;
    w.send(350, json!({"type":"clock"})).await;
    let second = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_ne!(first["recording"], second["recording"]);
    assert_eq!(second["audio_seconds"], 0.219);
    w.send(400, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert_eq!(batch_sizes(dir.path()), [299, 219]);
    assert!(!dir.path().join("pebble-index-rust/bluetooth.lock").exists());
    w.close().await;
}

#[tokio::test]
async fn repeated_short_singletons_after_long_each_emit_one_tap_without_asr() {
    // Replay the metadata from C2804..2808 on 2026-10-09. Different completed
    // sources own [Long], [Short], [Short], [Short], [Short, Short].
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), false).await;
    w.send(0, collection(1, 1, true, 250, 1, 1)).await;
    w.send(50, json!({"type":"clock"})).await;
    w.until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    let start = w.events.len();
    for index in 2..=5 {
        let time = index as u64 * 1000;
        let raw = collection(
            index,
            index as u32,
            true,
            4,
            0,
            if index == 5 { 2 } else { 1 },
        );
        w.send(time, raw.clone()).await;
        w.send(time + 1, raw).await; // Transport replay is still deduplicated.
        w.send(time + 300, json!({"type":"clock"})).await;
        assert_eq!(
            w.until(|v| v["type"] == "gesture").await["gesture"],
            "single_tap"
        );
    }
    w.send(6000, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    let events = &w.events[start..];
    assert_eq!(events.iter().filter(|v| v["type"] == "gesture").count(), 4);
    assert!(
        events
            .iter()
            .all(|v| v["type"] != "text" && v["type"] != "reception_activity")
    );
    assert_eq!(batch_sizes(dir.path()), [250]);
    w.close().await;
}

#[tokio::test]
async fn taps_and_replays_never_create_recording_ui_or_speech_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), true).await;
    w.send(10, collection(1, 1, true, 4, 0, 1)).await;
    w.send(60, json!({"type":"clock"})).await;
    assert_eq!(
        w.until(|v| v["type"] == "gesture").await["gesture"],
        "single_tap"
    );
    w.send(65, collection(1, 1, true, 4, 0, 1)).await; // Replay with a fresh transport sequence.
    w.send(100, collection(2, 2, true, 4, 0, 2)).await;
    w.send(130, collection(3, 3, true, 4, 0, 3)).await;
    assert_eq!(
        w.until(|v| v["type"] == "gesture").await["gesture"],
        "double_tap"
    );
    w.send(200, json!({"type":"clock"})).await;
    w.send(201, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert!(
        w.events
            .iter()
            .all(|v| v["type"] != "text" && v["type"] != "reception_activity")
    );
    assert_eq!(
        w.events.iter().filter(|v| v["type"] == "gesture").count(),
        2
    );
    assert!(batch_sizes(dir.path()).is_empty());
    w.close().await;
}

#[tokio::test]
async fn disconnect_and_empty_final_do_not_drop_late_audio_or_force_a_new_recording() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), false).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(51, json!({"type":"clock"})).await;
    w.send(60, collection(1, 1, false, 250, 0, 0)).await;
    w.send(70, json!({"type":"connection_lost"})).await;
    w.send(60_000, json!({"type":"clock"})).await;
    w.send(60_010, json!({"type":"connected"})).await;
    w.send(60_020, collection(3, 1, true, 0, 1, 1)).await; // final before missing middle.
    w.send(
        60_030,
        json!({"type":"button_state","pressed":false,"unread":2}),
    )
    .await;
    w.send(60_080, json!({"type":"clock"})).await;
    w.send(60_100, collection(2, 1, false, 23, 0, 0)).await;
    let final_text = w
        .until(|v| v["type"] == "text" && v["mode"] == "batch")
        .await;
    assert_eq!(final_text["audio_seconds"], 0.273);
    assert_eq!(batch_sizes(dir.path()), [273]);
    assert!(!w.events.iter().any(|v| v["type"] == "cancelled"));
    w.send(60_150, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    w.close().await;
}

#[tokio::test]
async fn known_unparsed_tap_is_classified_before_the_single_timer_fires() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), true).await;
    w.send(10, collection(1, 1, true, 4, 0, 1)).await;
    w.send(30, json!({"type":"range","start":1,"end":3})).await;
    w.send(60, json!({"type":"clock"})).await;
    w.send(300, collection(2, 2, true, 4, 0, 2)).await;
    assert_eq!(
        w.until(|v| v["type"] == "gesture").await["gesture"],
        "double_tap"
    );
    w.send(400, json!({"type":"clock"})).await;
    w.send(401, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert_eq!(
        w.events.iter().filter(|v| v["type"] == "gesture").count(),
        1
    );
    assert!(batch_sizes(dir.path()).is_empty());
    w.close().await;
}

#[tokio::test]
async fn count_hint_before_a_slow_range_read_preserves_the_double_tap() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), true).await;
    w.send(10, collection(1, 1, true, 4, 0, 1)).await;
    w.send(
        30,
        json!({"type":"button_state","pressed":false,"unread":2,"range_pending":true}),
    )
    .await;
    w.send(60, json!({"type":"clock"})).await;
    w.send(250, json!({"type":"range","start":1,"end":3})).await;
    w.send(300, collection(2, 2, true, 4, 0, 2)).await;
    assert_eq!(
        w.until(|v| v["type"] == "gesture").await["gesture"],
        "double_tap"
    );
    w.send(400, json!({"type":"clock"})).await;
    w.send(401, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert_eq!(
        w.events.iter().filter(|v| v["type"] == "gesture").count(),
        1
    );
    assert!(batch_sizes(dir.path()).is_empty());
    w.close().await;
}

#[tokio::test]
async fn a_gap_before_short_final_is_drained_before_the_source_is_retired() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = Worker::new(dir.path(), true).await;
    w.send(1, json!({"type":"button_state","pressed":true,"unread":1}))
        .await;
    w.send(10, collection(1, 1, false, 1, 0, 0)).await;
    w.send(
        20,
        json!({"type":"button_state","pressed":false,"unread":2}),
    )
    .await;
    w.send(25, collection(3, 1, true, 0, 0, 1)).await;
    w.send(30, collection(2, 1, false, 3, 0, 0)).await;
    w.send(80, json!({"type":"clock"})).await;
    assert_eq!(
        w.until(|v| v["type"] == "gesture").await["gesture"],
        "single_tap"
    );
    w.send(81, json!({"type":"flush"})).await;
    w.until(|v| v["type"] == "flushed").await;
    assert!(
        w.events
            .iter()
            .all(|v| v["type"] != "reception_activity" && v["type"] != "text")
    );
    w.close().await;
}
