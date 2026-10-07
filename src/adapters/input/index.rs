use super::continuation::Continuation;
use super::gestures::{Detector, Hooks, LogHook, Press};
use super::{AudioChunk, InputAdapter, InputEvent, pcm::PcmInput};
use crate::{config, output::Output, recordings::Recordings};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub struct IndexInput {
    recordings: Recordings,
    address: String,
    save_cursor: bool,
    continuation: Continuation,
    gestures: Detector,
    gesture_hooks: Hooks,
    gestures_live: bool,
}
impl IndexInput {
    pub fn new(address: &str, save_cursor: bool) -> Result<Self> {
        let ms = std::env::var("INDEX_VOICE_RESUME_MS")
            .unwrap_or_else(|_| "500".into())
            .parse::<u64>()
            .context("INDEX_VOICE_RESUME_MS must be an integer from 0 to 5000")?;
        ensure!(ms <= 5000, "INDEX_VOICE_RESUME_MS must be from 0 to 5000");
        let mut gesture_hooks = Hooks::default();
        gesture_hooks.register(LogHook);
        Ok(Self {
            recordings: Recordings::default(),
            address: address.into(),
            save_cursor,
            gestures: Detector::new(Duration::from_millis(500)),
            gesture_hooks,
            gestures_live: false,
            continuation: Continuation::new(Duration::from_millis(if save_cursor {
                ms
            } else {
                0
            })),
        })
    }
}
impl IndexInput {
    fn decode_raw(&mut self, message: Value, output: &Output) -> Result<Vec<InputEvent>> {
        match message["type"].as_str().context("Missing input type")? {
            "collection" => {
                let started = Instant::now();
                let index = u16::try_from(message["index"].as_u64().context("Missing index")?)?;
                let raw =
                    STANDARD.decode(message["raw"].as_str().context("Missing raw collection")?)?;
                let parts = self.recordings.add(index, &raw, output)?;
                if self.gestures_live {
                    // Use explicit button metadata plus the existing short-audio
                    // classification. Do not count cumulative sequence entries
                    // again every time they appear in an audio collection.
                    let records = crate::collection::records(&raw)?;
                    let short = records.get(&83).is_some_and(|bytes| {
                        if bytes.len() < 8 {
                            return false;
                        }
                        let count = crate::collection::u32le(&bytes[4..]);
                        count > 0
                            && count <= 32
                            && crate::collection::u32le(bytes) & (1 << (count - 1)) == 0
                    });
                    let press =
                        if short && parts.iter().any(|p| p.final_part && p.samples.is_empty()) {
                            Some(Press::Short)
                        } else if parts.iter().any(|p| !p.samples.is_empty()) {
                            Some(Press::Hold)
                        } else {
                            None
                        };
                    if let Some(press) = press {
                        if let Some(event) = self.gestures.observe(index, press, Instant::now()) {
                            self.gesture_hooks.dispatch(event, output);
                        }
                    }
                }
                output.debug(format!(
                    "decoder collection={index} decode={:.3}s",
                    started.elapsed().as_secs_f64()
                ));
                Ok(parts
                    .into_iter()
                    .map(|part| {
                        InputEvent::Audio(AudioChunk {
                            key: part.key,
                            samples: part.samples,
                            rate: part.rate,
                            final_part: part.final_part,
                            checkpoint: Some(json!(part.next)),
                        })
                    })
                    .collect())
            }
            "boundary" => {
                self.gestures.reset();
                self.gestures_live = false;
                self.recordings.reset(u16::try_from(
                    message["index"].as_u64().context("Missing boundary")?,
                )?);
                Ok(vec![])
            }
            "range" => Ok(self
                .recordings
                .retain(
                    u16::try_from(message["start"].as_u64().context("Missing start")?)?,
                    u16::try_from(message["end"].as_u64().context("Missing end")?)?,
                    output,
                )
                .into_iter()
                .map(InputEvent::Discard)
                .collect()),
            _ => PcmInput.decode(message, output),
        }
    }
}
impl InputAdapter for IndexInput {
    fn decode(&mut self, message: Value, output: &Output) -> Result<Vec<InputEvent>> {
        match message["type"].as_str() {
            Some("connection_lost") => {
                self.gestures.reset();
                self.gestures_live = false;
                return Ok(self.continuation.connection_lost(output));
            }
            Some("caught_up") => {
                self.continuation.caught_up(output);
                self.gestures_live = self.save_cursor;
                return Ok(vec![]);
            }
            _ => {}
        }
        let mut result = vec![];
        for event in self.decode_raw(message, output)? {
            result.extend(self.continuation.apply(event, output)?);
        }
        Ok(result)
    }
    fn poll(&mut self, output: &Output) -> Result<Vec<InputEvent>> {
        if self.gestures_live {
            if let Some(event) = self.gestures.poll(Instant::now()) {
                self.gesture_hooks.dispatch(event, output);
            }
        }
        Ok(self.continuation.poll(output))
    }
    fn commit(&mut self, checkpoint: &Value) -> Result<()> {
        if self.save_cursor {
            config::save_cursor(
                &self.address,
                u16::try_from(checkpoint.as_u64().context("Invalid Index checkpoint")?)?,
            )?;
        }
        Ok(())
    }
}
