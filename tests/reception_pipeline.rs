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
        let engine = dir.join("mock-asr");
        std::fs::write(&engine, r#"#!/bin/sh
printf '{"type":"ready"}\n'
while IFS= read -r line; do
printf '%s\t%s\n' "$2" "$line" >> "$ENGINE_LOG"
case "$line" in
*'"type":"audio"'*) printf '{"type":"accepted"}\n'; if [ "$2" = live ]; then printf '{"type":"partial","text":"live words"}\n'; fi;;
*'"type":"finish"'*) printf '{"type":"final","text":"whole recording"}\n';;
*'"type":"cancel"'*) printf '{"type":"cancelled"}\n';;
esac
done
"#).unwrap();
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
