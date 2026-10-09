//! Plays the assistant's PCM through the AudioPlayback Swift helper and
//! tracks how much of it has been heard, for `conversation.item.truncate`.
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use pebble_core::{
    helper::{Helper, HelperInput, executable},
    output::Output,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::process::Command;

pub struct Playback {
    input: HelperInput,
    played_ms: Arc<AtomicU64>,
    /// Audio handed to the helper since the last clear; bounds `played_ms`.
    queued_ms: AtomicU64,
    _reader: tokio::task::JoinHandle<()>,
}
impl Playback {
    pub async fn start(output: Output) -> Result<Self> {
        let program = executable(
            "AudioPlayback",
            include_str!("../native/AudioPlayback.swift"),
            &output,
        )
        .await?;
        let mut helper = Helper::spawn(Command::new(program), output.clone(), "playback".into())
            .await
            .context("Start audio playback helper")?;
        let ready = helper.event().await?;
        anyhow::ensure!(ready["type"] == "ready", "Playback helper failed: {ready}");
        let input = helper.input();
        let played_ms = Arc::new(AtomicU64::new(0));
        let played = played_ms.clone();
        let reader = tokio::spawn(async move {
            while let Ok(event) = helper.event().await {
                match event["type"].as_str() {
                    Some("played") => {
                        played.store(event["ms"].as_u64().unwrap_or(0), Ordering::SeqCst);
                    }
                    Some("error") => output.error(format!("Playback: {}", event["text"])),
                    _ => {}
                }
            }
        });
        Ok(Self {
            input,
            played_ms,
            queued_ms: AtomicU64::new(0),
            _reader: reader,
        })
    }
    pub async fn push(&self, pcm: &[i16], rate: u32) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.queued_ms
            .fetch_add(pcm.len() as u64 * 1000 / rate as u64, Ordering::SeqCst);
        self.input
            .send(&json!({"type": "audio", "rate": rate, "pcm": STANDARD.encode(bytes)}))
            .await
    }
    /// Stop immediately and drop queued audio.
    pub async fn clear(&self) -> Result<()> {
        self.played_ms.store(0, Ordering::SeqCst);
        self.queued_ms.store(0, Ordering::SeqCst);
        self.input.send(&json!({"type": "clear"})).await
    }
    /// Milliseconds actually heard, never past the audio queued so far.
    pub fn played_ms(&self) -> u64 {
        self.played_ms
            .load(Ordering::SeqCst)
            .min(self.queued_ms.load(Ordering::SeqCst))
    }
}
