use anyhow::{Result, ensure};
use serde_json::Value;
use std::time::Duration;
use tokio::process::Command;

#[tokio::test]
#[ignore = "Runs SpeechAnalyzer and AVFoundation on temporary synthesized audio; no BLE"]
async fn file_transcription_and_all_logs() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(90), verify()).await?
}
async fn verify() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("source.aiff");
    let text = temp.path().join("result.txt");
    let wav = temp.path().join("decoded.wav");
    let log = temp.path().join("run.log");
    ensure!(
        Command::new("say")
            .args(["-v", "Kyoko", "-o"])
            .arg(&input)
            .arg("こんにちは。これは音声認識の動作確認です。")
            .status()
            .await?
            .success(),
        "say failed"
    );
    let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .arg("transcribe")
        .arg(&input)
        .arg("--text-output")
        .arg(&text)
        .arg("--wav-output")
        .arg(&wav)
        .arg("--log")
        .arg(&log)
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout)?;
    ensure!(
        value["text"].as_str().unwrap().contains("こんにちは"),
        "{value}"
    );
    ensure!(
        std::fs::read_to_string(text)?.trim() == value["text"].as_str().unwrap(),
        "Text file mismatch"
    );
    ensure!(
        std::fs::read(wav)?.starts_with(b"RIFF"),
        "Invalid WAV export"
    );
    let log = std::fs::read_to_string(log)?;
    ensure!(
        log.contains("SpeechAnalyzer") && log.contains("こんにちは") && log.contains("stop pid="),
        "Missing output in log"
    );
    Ok(())
}
