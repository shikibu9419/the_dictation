//! Plays the assistant's PCM through the AudioPlayback Swift helper and
//! tracks which item is under the play head and how much of it has been
//! heard, for `conversation.item.truncate`.
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use pebble_core::{
    helper::{Helper, HelperInput, executable},
    output::Output,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

pub struct Playback {
    input: HelperInput,
    position: Arc<Mutex<Option<(String, u64)>>>,
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
        let position = Arc::new(Mutex::new(None));
        let reported = position.clone();
        let reader = tokio::spawn(async move {
            while let Ok(event) = helper.event().await {
                match event["type"].as_str() {
                    Some("played") => {
                        if let (Some(item), Some(ms)) =
                            (event["item"].as_str(), event["ms"].as_u64())
                        {
                            *reported.lock().unwrap() = Some((item.to_owned(), ms));
                        }
                    }
                    Some("cleared") => *reported.lock().unwrap() = None,
                    Some("error") => output.error(format!("Playback: {}", event["text"])),
                    _ => {}
                }
            }
        });
        Ok(Self {
            input,
            position,
            _reader: reader,
        })
    }
    pub async fn push(&self, pcm: &[i16], rate: u32, item: &str) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.input
            .send(&json!({"type": "audio", "item": item, "rate": rate, "pcm": STANDARD.encode(bytes)}))
            .await
    }
    /// Stop immediately and drop queued audio.
    pub async fn clear(&self) -> Result<()> {
        *self.position.lock().unwrap() = None;
        self.input.send(&json!({"type": "clear"})).await
    }
    /// The item being heard and how many milliseconds of it have played.
    pub fn position(&self) -> Option<(String, u64)> {
        self.position.lock().unwrap().clone()
    }
}
