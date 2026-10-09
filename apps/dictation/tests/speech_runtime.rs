use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc,
};

fn collection(key: u32, final_part: bool, samples: &[i16]) -> Vec<u8> {
    let mut pcm = 9997u32.to_le_bytes().to_vec();
    for n in samples {
        pcm.extend(n.to_le_bytes());
    }
    let mut records = vec![80];
    records.extend((pcm.len() as u32).to_le_bytes());
    records.extend(pcm);
    records.push(82);
    records.extend(6u16.to_le_bytes());
    records.extend(key.to_le_bytes());
    records.extend([1, final_part as u8]);
    let mut raw = ((records.len() + 4) as u32).to_le_bytes().to_vec();
    raw.extend(records);
    raw
}
async fn send(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    stdin.write_all(format!("{value}\n").as_bytes()).await?;
    Ok(())
}
struct Group(i32);
impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[tokio::test]
#[ignore = "Runs real macOS SpeechAnalyzer with temporary synthesized audio; no BLE or microphone"]
async fn twenty_recordings_including_three_minutes() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(180), verify()).await?
}
async fn verify() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let aiff = temp.path().join("source.aiff");
    let wav = temp.path().join("source.wav");
    ensure!(
        Command::new("say")
            .args(["-v", "Kyoko", "-o"])
            .arg(&aiff)
            .arg("こんにちは。これは音声認識の動作確認です。")
            .status()
            .await?
            .success(),
        "say failed"
    );
    ensure!(
        Command::new("afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@9997", "-c", "1"])
            .arg(aiff)
            .arg(&wav)
            .status()
            .await?
            .success(),
        "afconvert failed"
    );
    let bytes = std::fs::read(wav)?;
    let mut cursor = 12;
    let mut unit = Vec::new();
    while cursor + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let start = cursor + 8;
        let end = start + size;
        ensure!(end <= bytes.len(), "Invalid WAV");
        if &bytes[cursor..cursor + 4] == b"data" {
            unit = bytes[start..end]
                .chunks_exact(2)
                .map(|p| i16::from_le_bytes([p[0], p[1]]))
                .collect();
            break;
        }
        cursor = end + (size % 2);
    }
    ensure!(!unit.is_empty(), "Empty synthetic audio");
    unit.resize(6 * 9997, 0);
    let mut child=Command::new(env!("CARGO_BIN_EXE_pebble-index")).arg("__worker")
        .arg(json!({"address":"integration-only","language":"ja-JP","command":"fetch","verbose":false}).to_string())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).process_group(0).kill_on_drop(true).spawn()?;
    let group = Group(child.id().unwrap() as i32);
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let partials = Arc::new(AtomicUsize::new(0));
    let observed = partials.clone();
    let reader = tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            let event: Value = serde_json::from_str(&line).unwrap();
            if event["type"] == "text" && event["final"] == false {
                observed.fetch_add(1, Ordering::Relaxed);
            }
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    let ready = rx.recv().await.context("Worker closed before ready")?;
    ensure!(ready["type"] == "ready", "{ready}");
    let mut index = 200u32;
    for number in 0..20 {
        let repeat = if number == 1 { 30 } else { 1 };
        let samples = unit.repeat(repeat);
        let key = index;
        send(&mut stdin, json!({"type":"state","collecting":true})).await?;
        let parts: Vec<_> = samples.chunks(1800).collect();
        for (n, chunk) in parts.iter().enumerate() {
            let final_part = n == parts.len() - 1;
            if final_part {
                if number == 0 {
                    ensure!(
                        partials.load(Ordering::Relaxed) > 0,
                        "No first live result before release"
                    );
                }
                send(&mut stdin, json!({"type":"state","collecting":false})).await?;
            }
            send(&mut stdin,json!({"type":"collection","index":index as u16,"raw":STANDARD.encode(collection(key,final_part,chunk))})).await?;
            index += 1;
            if repeat == 1 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    send(&mut stdin, json!({"type":"flush"})).await?;
    let mut finals = Vec::new();
    loop {
        let event = rx.recv().await.context("Worker exited before flush")?;
        ensure!(event["type"] != "error", "{event}");
        if event["type"] == "flushed" {
            break;
        }
        if event["type"] == "text" && event["final"] == true {
            finals.push(event);
        }
    }
    ensure!(
        finals.len() == 20,
        "Expected 20 final results, got {}",
        finals.len()
    );
    for (n, event) in finals.iter().enumerate() {
        let seconds = if n == 1 { 180.0 } else { 6.0 };
        ensure!(
            (event["audio_seconds"].as_f64().unwrap() - seconds).abs() < 0.001,
            "Input length: {event}"
        );
        ensure!(
            event["text"].as_str().unwrap().contains("こんにちは"),
            "No expected phrase: {event}"
        );
        let segments = event["segments"].as_array().unwrap();
        ensure!(!segments.is_empty(), "No final segments");
        ensure!(
            segments[0][0].as_f64().unwrap() < 1.0,
            "Leading audio missing: {event}"
        );
        ensure!(
            segments.last().unwrap()[1].as_f64().unwrap() >= seconds - 0.5,
            "Trailing audio missing: {event}"
        );
        for pair in segments.windows(2) {
            ensure!(
                pair[1][0].as_f64().unwrap() - pair[0][1].as_f64().unwrap() < 0.1,
                "Gap in final audio: {event}"
            );
        }
    }
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
    ensure!(status.success(), "Worker exit: {status}");
    reader.await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    ensure!(
        unsafe { libc::kill(-group.0, 0) } == -1,
        "Orphan helper process group"
    );
    eprintln!(
        "20 final results, {} live updates; all 0..180s covered; no orphan helpers",
        partials.load(Ordering::Relaxed)
    );
    Ok(())
}
