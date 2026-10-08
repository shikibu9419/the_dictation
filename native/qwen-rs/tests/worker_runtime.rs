//! Starts only QwenNative, never the application, BLE, microphone, or a window.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
struct Worker {
    child: Child,
    input: Option<ChildStdin>,
    events: mpsc::Receiver<(Instant, Value)>,
    seen: Vec<(Instant, Value)>,
}
impl Worker {
    fn start(mode: &str) -> Self {
        let model = std::env::var_os("INDEX_QWEN_MODEL").expect("Set INDEX_QWEN_MODEL");
        let mut child = Command::new(env!("CARGO_BIN_EXE_QwenNative"))
            .arg(model)
            .args(["ja_JP", mode])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, events) = mpsc::channel();
        let log = std::env::var_os("INDEX_QWEN_REPORT_DIR").map(|p| {
            std::fs::create_dir_all(&p).unwrap();
            std::fs::File::create(
                std::path::Path::new(&p).join(format!("{mode}-{}.jsonl", child.id())),
            )
            .unwrap()
        });
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut log = log;
            for line in BufReader::new(output).lines() {
                let line = line.unwrap();
                let event: Value =
                    serde_json::from_str(&line).expect("Worker stdout must contain only JSONL");
                if let Some(log) = &mut log {
                    writeln!(
                        log,
                        "{}",
                        json!({"elapsed":start.elapsed().as_secs_f64(),"event":event})
                    )
                    .unwrap();
                }
                if tx.send((Instant::now(), event)).is_err() {
                    break;
                }
            }
        });
        let mut worker = Self {
            child,
            input: Some(input),
            events,
            seen: Vec::new(),
        };
        let ready = worker.until("ready", Duration::from_secs(30));
        assert_eq!(ready["protocol_version"], 2);
        assert!(!worker
            .seen
            .iter()
            .any(|(_, v)| matches!(v["type"].as_str(), Some("partial" | "final"))));
        worker
    }
    fn send(&mut self, v: Value) {
        let i = self.input.as_mut().unwrap();
        writeln!(i, "{v}").unwrap();
        i.flush().unwrap();
    }
    fn audio(&mut self, pcm: &[i16], id: u64) {
        let bytes: Vec<u8> = pcm.iter().flat_map(|n| n.to_le_bytes()).collect();
        self.send(json!({"type":"audio","sample_rate":16000,"pcm":STANDARD.encode(bytes),"session_id":id,"generation":id}));
        let ack = self.until("accepted", Duration::from_secs(5));
        assert_eq!(ack["session_id"], id);
    }
    fn receive(&mut self, timeout: Duration) -> Value {
        let (at, v) = self
            .events
            .recv_timeout(timeout)
            .expect("Qwen worker event timed out");
        assert_ne!(v["type"], "error", "{v}");
        self.seen.push((at, v.clone()));
        v
    }
    fn until(&mut self, kind: &str, timeout: Duration) -> Value {
        let end = Instant::now() + timeout;
        loop {
            let v = self.receive(end.saturating_duration_since(Instant::now()));
            if v["type"] == kind {
                return v;
            }
        }
    }
    fn finish(&mut self, id: u64, samples: usize) -> String {
        self.send(json!({"type":"finish","session_id":id,"generation":id}));
        let v = self.until("final", Duration::from_secs(30));
        assert_eq!(v["session_id"], id);
        assert_eq!(v["consumed_samples"], samples as u64);
        v["text"].as_str().unwrap().into()
    }
    fn close(&mut self) {
        self.input.take();
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < end, "Worker did not stop on EOF");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn wav(name: &str) -> Vec<i16> {
    let root = std::env::var_os("INDEX_QWEN_FIXTURES").expect("Set INDEX_QWEN_FIXTURES");
    let mut wav = hound::WavReader::open(std::path::Path::new(&root).join(name)).unwrap();
    assert_eq!(wav.spec().sample_rate, 16000);
    assert_eq!(wav.spec().channels, 1);
    wav.samples::<i16>().map(Result::unwrap).collect()
}
#[test]
#[ignore = "Explicit model subprocess test, no UI"]
fn batch_pause_cancel_repeated_recordings_and_eof() {
    let short = wav("short.wav");
    let long = wav("long.wav");
    let mut worker = Worker::start("batch");
    worker.send(json!({"type":"permit","enabled":false}));
    for chunk in long.chunks(3200) {
        worker.audio(chunk, 1);
    }
    worker.send(json!({"type":"finish","session_id":1,"generation":1}));
    worker.send(json!({"type":"cancel","session_id":1,"generation":1}));
    assert_eq!(
        worker.until("cancelled", Duration::from_secs(2))["session_id"],
        1
    );
    worker.send(json!({"type":"permit","enabled":true}));
    for id in [2, 3] {
        for chunk in short.chunks(3200) {
            worker.audio(chunk, id);
        }
        assert_eq!(
            worker.finish(id, short.len()),
            "こんにちは。これは音声認識の動作確認です。"
        );
    }
    for chunk in long.chunks(3200) {
        worker.audio(chunk, 4);
    }
    let text = worker.finish(4, long.len());
    assert!(text.starts_with("最初の確認です。"), "{text}");
    assert!(text.ends_with("これで最後の確認を終わります。"), "{text}");
    let spans: Vec<(f64, f64)> = worker
        .seen
        .iter()
        .filter(|(_, v)| v["session_id"] == 4)
        .filter_map(|(_, v)| Some((v["segment_start"].as_f64()?, v["segment_end"].as_f64()?)))
        .collect();
    assert_eq!(spans.first().unwrap().0, 0.0);
    assert_eq!(spans.last().unwrap().1, long.len() as f64 / 16000.0);
    assert!(spans.windows(2).all(|v| v[0].1 == v[1].0));
    // Legacy clients still work with implicit IDs.
    worker.send(json!({"type":"finish"}));
    assert_eq!(worker.until("final", Duration::from_secs(5))["text"], "");
    worker.close();
}
#[test]
#[ignore = "Explicit pause/resume, silence and EOF subprocess test; no UI"]
fn paused_batch_resumes_without_losing_pcm_and_exits_on_eof() {
    let short = wav("short.wav");
    let mut worker = Worker::start("batch");
    worker.send(json!({"type":"permit","enabled":false}));
    for chunk in short.chunks(3200) {
        worker.audio(chunk, 1);
    }
    worker.send(json!({"type":"finish","session_id":1,"generation":1}));
    // accepted is independent of model execution: the paused worker has
    // acknowledged the entire recording but has published no result.
    assert!(!worker.seen.iter().any(|(_, v)| v["type"] == "final"));
    worker.send(json!({"type":"permit","enabled":true}));
    let result = worker.until("final", Duration::from_secs(15));
    assert_eq!(result["consumed_samples"], short.len());
    assert_eq!(result["text"], "こんにちは。これは音声認識の動作確認です。");
    worker.audio(&vec![0; 16000], 2);
    assert_eq!(worker.finish(2, 16000), "");
    worker.send(json!({"type":"permit","enabled":false}));
    worker.audio(&short, 3);
    worker.send(json!({"type":"finish","session_id":3,"generation":3}));
    worker.close();
}
#[test]
#[ignore = "75-second paced real-model stream; no UI, microphone, or audio playback"]
fn paced_long_live_stream_remains_bounded() {
    let source = wav("long.wav");
    let audio = [source.clone(), source].concat();
    let mut worker = Worker::start("live");
    let started = Instant::now();
    let mut sent = 0;
    for chunk in audio.chunks(3200) {
        sent += chunk.len();
        let due = started + Duration::from_secs_f64(sent as f64 / 16000.0);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        worker.audio(chunk, 1);
    }
    let text = worker.finish(1, audio.len());
    let elapsed = started.elapsed().as_secs_f64();
    assert!(text.contains("最初の確認です"), "{text}");
    assert!(text.ends_with("これで最後の確認を終わります。"), "{text}");
    let partials: Vec<_> = worker
        .seen
        .iter()
        .filter(|(_, v)| v["type"] == "partial")
        .collect();
    assert!(!partials.is_empty());
    let first = partials[0].0.duration_since(started).as_secs_f64();
    let mut early = Vec::new();
    let mut late = Vec::new();
    let mut peak = 0.0_f64;
    let mut previous_consumed = 0;
    let mut memory = 0;
    for (_, v) in &worker.seen {
        memory = memory.max(v["peak_memory_bytes"].as_u64().unwrap_or(0));
        if let Some(window) = v["window_samples"].as_u64() {
            assert!(window <= 30 * 16000);
        }
        if let (Some(a), Some(c)) = (
            v["accepted_samples"].as_u64(),
            v["consumed_samples"].as_u64(),
        ) {
            assert!(a >= c);
            assert!(
                c >= previous_consumed,
                "Recognition cursor moved backwards: {v}"
            );
            previous_consumed = c;
            let lag = (a - c) as f64 / 16000.0;
            peak = peak.max(lag);
            if a < 20 * 16000 {
                early.push(lag);
            } else if a > 55 * 16000 {
                late.push(lag);
            }
        }
    }
    early.sort_by(f64::total_cmp);
    late.sort_by(f64::total_cmp);
    assert!(!early.is_empty() && !late.is_empty());
    let early = early[early.len() / 2];
    let late = late[late.len() / 2];
    eprintln!(
        "{}",
        json!({"audio_seconds":audio.len()as f64/16000.0,"elapsed":elapsed,"first_partial_seconds":first,"peak_lag_seconds":peak,"early_median_lag":early,"late_median_lag":late,"peak_memory_bytes":memory,"text":text})
    );
    assert!(
        late <= early + 1.0,
        "Recognition lag accumulated: early={early}, late={late}"
    );
    assert!(first < 8.0, "First partial delayed: {first}");
    // A new recording must have clean model/PCM/prefix state.
    let short = wav("short.wav");
    for chunk in short.chunks(3200) {
        worker.audio(chunk, 2);
    }
    assert_eq!(
        worker.finish(2, short.len()),
        "こんにちは。これは音声認識の動作確認です。"
    );
    worker.close();
}
