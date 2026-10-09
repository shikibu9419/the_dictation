use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::process::Command;

#[tokio::test]
#[ignore = "Uses installed Whisper large-v3 model on synthesized Japanese audio; no microphone or BLE"]
async fn whisper_adapter_transcribes_whole_file() -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(180),
        verify(
            "こんにちは。今日は東京の天気について話します。最後に明日の予定を確認します。",
            &["こんにちは", "東京", "予定"],
        ),
    )
    .await?
}
async fn verify(spoken: &str, expected: &[&str]) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("pebble-index-rust");
    std::fs::create_dir(&config)?;
    let model = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join("Library/Caches/pebble-index-rust/ggml-large-v3.bin");
    ensure!(model.is_file(), "Run download-model first");
    std::fs::write(
        config.join("settings.json"),
        serde_json::to_vec(&json!({"speech":"whisper_large_v3","whisper_model":model}))?,
    )?;
    let input = temp.path().join("speech.aiff");
    ensure!(
        Command::new("say")
            .args(["-v", "Kyoko", "-o"])
            .arg(&input)
            .arg(spoken)
            .status()
            .await?
            .success(),
        "say failed"
    );
    let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .arg("transcribe")
        .arg(input)
        .env("XDG_CONFIG_HOME", temp.path())
        .env_remove("INDEX_VOICE_SPEECH_COMMAND")
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout)?;
    let text = value["text"].as_str().unwrap_or("");
    ensure!(
        expected.iter().all(|word| text.contains(word)),
        "Incomplete Whisper result: {text}"
    );
    eprintln!("Whisper full result: {text}");
    Ok(())
}

#[tokio::test]
#[ignore = "Exercises real large-v3 live cancellation and session reuse on synthesized audio"]
async fn whisper_live_cancel_then_second_recording() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(180), verify_live()).await?
}
async fn verify_live() -> Result<()> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("source.aiff");
    let wav = temp.path().join("source.wav");
    ensure!(
        Command::new("say")
            .args(["-v", "Kyoko", "-o"])
            .arg(&input)
            .arg("こんにちは。今日は東京の天気について話します。最後に明日の予定を確認します。")
            .status()
            .await?
            .success(),
        "say failed"
    );
    ensure!(
        Command::new("afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .arg(input)
            .arg(&wav)
            .status()
            .await?
            .success(),
        "convert failed"
    );
    let wav = std::fs::read(wav)?;
    let mut pos = 12;
    let mut pcm = None;
    while pos + 8 <= wav.len() {
        let length = u32::from_le_bytes(wav[pos + 4..pos + 8].try_into()?) as usize;
        if &wav[pos..pos + 4] == b"data" {
            pcm = Some(&wav[pos + 8..pos + 8 + length]);
            break;
        }
        pos += 8 + length + (length % 2);
    }
    let pcm = pcm.ok_or_else(|| anyhow::anyhow!("Missing PCM"))?;
    let model = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join("Library/Caches/pebble-index-rust/ggml-large-v3.bin");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .arg("__whisper")
        .arg(model)
        .args(["ja-JP", "live"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    async fn wait(
        lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        kind: &str,
    ) -> Result<Value> {
        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow::anyhow!("Worker closed"))?;
            let value: Value = serde_json::from_str(&line)?;
            ensure!(value["type"] != "error", "{value}");
            if value["type"] == kind {
                return Ok(value);
            }
        }
    }
    wait(&mut lines, "ready").await?;
    for n in 0..3 {
        input
            .write_all(
                format!(
                    "{}\n",
                    json!({"type":"audio","sample_rate":16000,"pcm":STANDARD.encode(pcm)})
                )
                .as_bytes(),
            )
            .await?;
        wait(&mut lines, "accepted").await?;
        let live = wait(&mut lines, "partial").await?;
        ensure!(
            live["text"]
                .as_str()
                .is_some_and(|t| t.contains("こんにちは")),
            "Missing live result {n}: {live}"
        );
        input.write_all(b"{\"type\":\"cancel\"}\n").await?;
        wait(&mut lines, "cancelled").await?;
        input.write_all(b"{\"type\":\"finish\"}\n").await?;
        let empty = wait(&mut lines, "final").await?;
        ensure!(
            empty["text"] == "",
            "Cancelled audio leaked into next recording: {empty}"
        );
    }
    drop(input);
    ensure!(child.wait().await?.success(), "Worker did not exit cleanly");
    Ok(())
}

#[tokio::test]
#[ignore = "Checks large-v3 coverage beyond a 30-second decoder window"]
async fn whisper_long_recording_keeps_start_and_end() -> Result<()> {
    let middle =
        "今日は静かな公園を散歩しました。大きな木の下で休んでから、図書館で本を読みました。"
            .repeat(8);
    let spoken = format!(
        "最初に北海道の雪について説明します。{middle}最後に沖縄の海について確認して終了します。"
    );
    tokio::time::timeout(
        Duration::from_secs(240),
        verify(&spoken, &["北海道", "公園", "沖縄"]),
    )
    .await?
}
