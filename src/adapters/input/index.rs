use super::{AudioChunk, InputAdapter, InputEvent, pcm::PcmInput};
use crate::{config, output::Output, recordings::Recordings};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::Instant;

pub struct IndexInput {
    recordings: Recordings,
    address: String,
    save_cursor: bool,
}
impl IndexInput {
    pub fn new(address: &str, save_cursor: bool) -> Self {
        Self {
            recordings: Recordings::default(),
            address: address.into(),
            save_cursor,
        }
    }
}
impl InputAdapter for IndexInput {
    fn decode(&mut self, message: Value, output: &Output) -> Result<Vec<InputEvent>> {
        match message["type"].as_str().context("Missing input type")? {
            "collection" => {
                let started = Instant::now();
                let index = u16::try_from(message["index"].as_u64().context("Missing index")?)?;
                let raw =
                    STANDARD.decode(message["raw"].as_str().context("Missing raw collection")?)?;
                let parts = self.recordings.add(index, &raw, output)?;
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
