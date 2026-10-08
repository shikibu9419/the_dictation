//! Headless IPC validation with a counting ASR stub; no audio devices/models.
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

fn engine(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("counting-asr");
    std::fs::write(&path, r#"#!/bin/sh
exec /usr/bin/awk -v mode="$2" '
BEGIN { print "{\"type\":\"ready\"}"; fflush() }
/"type":"audio"/ {
  pcm=$0; sub(/.*"pcm":"/, "", pcm); sub(/".*/, "", pcm)
  n=length(pcm)*3/4; if (pcm ~ /==$/) n-=2; else if (pcm ~ /=$/) n-=1
  bytes+=n; print "{\"type\":\"accepted\"}"; fflush()
}
/"type":"finish"/ { printf "{\"type\":\"final\",\"text\":\"%s:%d\"}\n", mode, bytes; bytes=0; fflush() }
/"type":"cancel"/ { bytes=0; print "{\"type\":\"cancelled\"}"; fflush() }
'
"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn a_file_larger_than_one_json_frame_reaches_batch_without_truncation_or_live_work() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let audio = dir.path().join("long.pcm");
    let bytes = 13 * 1024 * 1024; // Its base64 form exceeds the 16 MiB JSONL limit.
    std::fs::File::create(&audio)
        .unwrap()
        .set_len(bytes)
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_pebble-index"))
            .arg("transcribe")
            .arg(&audio)
            .args(["--raw-sample-rate", "192000"])
            .env("XDG_CONFIG_HOME", dir.path())
            .env("INDEX_VOICE_SPEECH_COMMAND", engine)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["text"], format!("batch:{bytes}"));
}

#[tokio::test]
async fn oversized_unterminated_input_returns_error_without_acknowledging_a_final() {
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let options = json!({"address":"pcm", "language":"ja_JP", "command":"stream", "verbose":false});
    let mut child = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .arg("__worker")
        .arg(options.to_string())
        .env("XDG_CONFIG_HOME", dir.path())
        .env("INDEX_VOICE_SPEECH_COMMAND", engine)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if serde_json::from_str::<Value>(&line).unwrap()["type"] == "ready" {
                break;
            }
        }
        let _ = child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&vec![b'x'; pebble_index::ipc::MAX_LINE_BYTES + 1])
            .await;
        let mut failed = false;
        while let Some(line) = output.next_line().await.unwrap() {
            let event: Value = serde_json::from_str(&line).unwrap();
            assert_ne!(event["final"], true);
            if event["type"] == "error" {
                assert!(
                    event["text"].as_str().unwrap().contains("IPC line exceeds"),
                    "{event}"
                );
                failed = true;
            }
        }
        assert!(failed);
        assert!(!child.wait().await.unwrap().success());
    })
    .await
    .unwrap();
}
