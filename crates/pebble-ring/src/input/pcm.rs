use super::{AudioChunk, InputAdapter, InputEvent};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use pebble_core::output::Output;
use serde_json::Value;

pub struct PcmInput;
impl InputAdapter for PcmInput {
    fn decode(&mut self, message: Value, _: &Output) -> Result<Vec<InputEvent>> {
        let kind = message["type"].as_str().context("Missing input type")?;
        Ok(vec![match kind {
            "state" => InputEvent::State(message["collecting"].as_bool().context("Missing state")?),
            "flush" => InputEvent::Flush,
            "discard" => {
                InputEvent::Discard(message["key"].as_str().context("Missing key")?.into())
            }
            "recording" | "audio" => {
                let rate = u32::try_from(message["rate"].as_u64().context("Missing sample rate")?)?;
                ensure!((1000..=192000).contains(&rate), "Invalid sample rate");
                let bytes = STANDARD.decode(message["pcm"].as_str().context("Missing PCM")?)?;
                ensure!(bytes.len() % 2 == 0, "Invalid mono s16le PCM");
                InputEvent::Audio(AudioChunk {
                    key: message["key"]
                        .as_str()
                        .context("Missing recording key")?
                        .into(),
                    samples: bytes
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect(),
                    rate,
                    final_part: kind == "recording" || message["final"].as_bool().unwrap_or(false),
                    checkpoint: None,
                })
            }
            other => bail!("Unknown PCM input message: {other}"),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn decode(value: Value) -> Result<Vec<InputEvent>> {
        PcmInput.decode(value, &Output::new(false, None).unwrap())
    }
    #[test]
    fn audio_preserves_signed_pcm_and_identity() {
        let events=decode(json!({"type":"audio","key":"mic-one","rate":16000,"pcm":STANDARD.encode([0,128,255,127]),"final":false})).unwrap();
        let InputEvent::Audio(chunk) = &events[0] else {
            panic!("audio")
        };
        assert_eq!(chunk.key, "mic-one");
        assert_eq!(
            chunk.samples.iter().copied().collect::<Vec<_>>(),
            [-32768, 32767]
        );
        assert!(!chunk.final_part);
        assert!(chunk.checkpoint.is_none());
    }
    #[test]
    fn release_and_empty_final_are_independent() {
        assert!(matches!(
            decode(json!({"type":"state","collecting":false})).unwrap()[0],
            InputEvent::State(false)
        ));
        let events =
            decode(json!({"type":"audio","key":"mic-one","rate":16000,"pcm":"","final":true}))
                .unwrap();
        let InputEvent::Audio(chunk) = &events[0] else {
            panic!("audio")
        };
        assert!(chunk.final_part);
        assert!(chunk.samples.is_empty());
    }
    #[test]
    fn rejects_invalid_rates_odd_pcm_and_unknown_commands() {
        for rate in [0u64, 999, 192001, u64::MAX] {
            assert!(decode(json!({"type":"recording","key":"x","rate":rate,"pcm":""})).is_err());
        }
        assert!(decode(json!({"type":"audio","key":"x","rate":16000,"pcm":"AA=="})).is_err());
        assert!(decode(json!({"type":"unexpected"})).is_err());
    }
    #[test]
    fn complete_file_is_final_and_flush_is_forwarded() {
        let events =
            decode(json!({"type":"recording","key":"file","rate":16000,"pcm":"AAA="})).unwrap();
        let InputEvent::Audio(chunk) = &events[0] else {
            panic!("audio")
        };
        assert!(chunk.final_part);
        assert!(matches!(
            decode(json!({"type":"flush"})).unwrap()[0],
            InputEvent::Flush
        ));
    }
}
