use crate::{
    Transcribe,
    helper::executable,
    output::Output,
    recognition::{Client, Options},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::Path;
use tokio::process::Command;

pub struct FileRecognizer {
    client: Client,
    language: String,
    output: Output,
    next: u64,
}
impl FileRecognizer {
    pub async fn start(language: &str, output: Output) -> Result<Self> {
        let client = Client::start(
            Options {
                address: "file".into(),
                language: language.into(),
                command: "fetch".into(),
                verbose: output.verbose,
            },
            output.clone(),
        )
        .await?;
        Ok(Self {
            client,
            language: language.into(),
            output,
            next: 0,
        })
    }
    pub async fn recognize(
        &mut self,
        path: &Path,
        raw_rate: Option<u32>,
        wav: Option<&Path>,
    ) -> Result<Value> {
        let (pcm, rate) = if let Some(rate) = raw_rate {
            ensure!((1000..=192000).contains(&rate), "Invalid audio sample rate");
            ensure!(
                std::fs::metadata(path)?.len() <= 128 * 1024 * 1024,
                "Raw audio exceeds 64 Mi samples"
            );
            let pcm = std::fs::read(path)?;
            ensure!(
                !pcm.is_empty() && pcm.len() % 2 == 0,
                "Expected nonempty mono PCM 16-bit"
            );
            (pcm, rate)
        } else {
            let exe = executable(
                "AudioDecode",
                include_str!("../native/AudioDecode.swift"),
                &self.output,
            )
            .await?;
            let decoded = Command::new(exe)
                .arg(path)
                .kill_on_drop(true)
                .output()
                .await?;
            let value: Value =
                serde_json::from_slice(&decoded.stdout).context("Invalid decoded audio")?;
            ensure!(
                decoded.status.success() && value["type"] == "audio",
                "Audio decode failed: {value}"
            );
            (
                STANDARD.decode(value["pcm"].as_str().context("Missing PCM")?)?,
                value["rate"].as_u64().context("Missing sample rate")? as u32,
            )
        };
        ensure!(
            (1000..=192000).contains(&rate) && !pcm.is_empty() && pcm.len() % 2 == 0,
            "Expected nonempty mono s16le PCM with a valid sample rate"
        );
        ensure!(
            pcm.len() <= 128 * 1024 * 1024,
            "Decoded audio exceeds 64 Mi samples"
        );
        if let Some(path) = wav {
            write_wav(path, &pcm, rate)?;
        }
        self.next += 1;
        let key = format!("file-{}", self.next);
        self.client
            .send(json!({"type":"state","collecting":false}))?;
        // Keep file input within the same bounded JSONL protocol as live capture.
        let chunks = pcm.len().div_ceil(1024 * 1024);
        for (index, chunk) in pcm.chunks(1024 * 1024).enumerate() {
            self.client.send(json!({"type":"audio","key":key,"pcm":STANDARD.encode(chunk),"rate":rate,"final":index + 1 == chunks}))?;
            tokio::task::yield_now().await;
        }
        loop {
            let event = self
                .client
                .events
                .recv()
                .await?
                .context("Recognition worker closed")??;
            if event["type"] == "error" {
                bail!("Speech recognition: {}", event["text"]);
            }
            if event["type"] == "text" && event["final"] == true && event["recording"] == key {
                return Ok(json!({"language":self.language,"text":event["text"]}));
            }
        }
    }
}
fn write_wav(path: &Path, pcm: &[u8], rate: u32) -> Result<()> {
    let size = u32::try_from(pcm.len())?;
    ensure!(size <= u32::MAX - 36, "WAV is too large");
    let mut bytes = Vec::with_capacity(pcm.len() + 44);
    bytes.extend(b"RIFF");
    bytes.extend((size + 36).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(size.to_le_bytes());
    bytes.extend(pcm);
    std::fs::write(path, bytes)?;
    Ok(())
}
pub async fn transcribe(args: Transcribe, output: Output) -> Result<()> {
    let mut engine = FileRecognizer::start(&args.language, output.clone()).await?;
    let mut value = engine
        .recognize(&args.file, args.raw_sample_rate, args.wav_output.as_deref())
        .await?;
    value["source"] = json!(args.file);
    output.line(serde_json::to_string(&value)?);
    if let Some(path) = args.text_output {
        std::fs::write(path, format!("{}\n", value["text"].as_str().unwrap_or("")))?;
    }
    Ok(())
}
