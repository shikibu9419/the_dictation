pub mod gesture_types;
pub mod gestures;
pub mod index;
pub mod interaction;
pub mod pcm;

use anyhow::Result;
use pebble_core::output::Output;
use serde_json::Value;

#[derive(Clone)]
pub struct AudioChunk {
    pub key: String,
    pub samples: pebble_core::pcm::Pcm,
    pub rate: u32,
    pub final_part: bool,
    pub checkpoint: Option<Value>,
}
pub enum InputEvent {
    Gesture {
        event: gesture_types::GestureEvent,
        cancel_recording: Option<String>,
    },
    Audio(AudioChunk),
    State(bool),
    Reception {
        namespace: String,
        effect: crate::reception::input_effects::Effect,
    },
    Level {
        key: String,
        level: f64,
    },
    Discard(String),
    Flush,
    Checkpoint(Value),
}

/// Source-specific decoding and persistence stay on this side of the PCM boundary.
pub trait InputAdapter: Send {
    fn decode(&mut self, message: Value, output: &Output) -> Result<Vec<InputEvent>>;
    fn poll(&mut self, _output: &Output) -> Result<Vec<InputEvent>> {
        Ok(vec![])
    }
    fn completed(&mut self, _key: &str) {}
    /// EOF must not fabricate final audio or advance gesture deadlines.
    fn end_input(&self) -> Result<()> {
        Ok(())
    }
    fn commit(&mut self, _checkpoint: &Value) -> Result<()> {
        Ok(())
    }
}

/// Application policy that shapes ring input without exposing app settings.
#[derive(Clone, Copy, Debug)]
pub struct RingInputConfig {
    pub reception: crate::reception::config::Reception,
    /// When false, a short press completes immediately instead of waiting for a second tap.
    pub double_tap_enabled: bool,
    /// Emit `Effect::Live` chunks while the button is held.
    pub live_mode: bool,
}
impl Default for RingInputConfig {
    fn default() -> Self {
        Self {
            reception: crate::reception::config::Reception::default(),
            double_tap_enabled: true,
            live_mode: true,
        }
    }
}

pub fn create(
    address: &str,
    command: &str,
    config: RingInputConfig,
) -> Result<Box<dyn InputAdapter>> {
    Ok(if address == "file" || address == "pcm" {
        Box::new(pcm::PcmInput)
    } else {
        Box::new(index::IndexInput::new(
            address,
            command == "listen",
            config,
        )?)
    })
}
