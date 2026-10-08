mod continuation;
pub mod gestures;
mod index;
mod interaction;
mod pcm;
pub mod stream;

use crate::output::Output;
use anyhow::Result;
use serde_json::Value;

pub struct AudioChunk {
    pub key: String,
    pub samples: Vec<i16>,
    pub rate: u32,
    pub final_part: bool,
    pub checkpoint: Option<Value>,
}
pub enum InputEvent {
    Gesture(gestures::GestureEvent),
    Audio(AudioChunk),
    State(bool),
    Activity { key: String, collecting: bool },
    Discard(String),
    Flush,
    Checkpoint(Value),
    Interrupted(String),
    Cancel(String),
}

/// Source-specific decoding and persistence stay on this side of the PCM boundary.
pub trait InputAdapter: Send {
    fn decode(&mut self, message: Value, output: &Output) -> Result<Vec<InputEvent>>;
    fn poll(&mut self, _output: &Output) -> Result<Vec<InputEvent>> {
        Ok(vec![])
    }
    fn completed(&mut self, _key: &str) {}
    fn commit(&mut self, _checkpoint: &Value) -> Result<()> {
        Ok(())
    }
}

pub fn create(address: &str, command: &str) -> Result<Box<dyn InputAdapter>> {
    Ok(if address == "file" || address == "pcm" {
        Box::new(pcm::PcmInput)
    } else {
        Box::new(index::IndexInput::new(address, command == "listen")?)
    })
}
