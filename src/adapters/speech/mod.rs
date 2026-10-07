mod process;
pub mod whisper;

use crate::output::Output;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};

type Reply<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub enum EngineCommand {
    Audio { samples: Vec<i16>, rate: u32 },
    Finish,
    Cancel,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Ready,
    Accepted,
    Cancelled,
    Partial {
        text: String,
    },
    Final {
        text: String,
    },
    Status {
        text: String,
        segment_start: Option<f64>,
        segment_end: Option<f64>,
    },
    Error {
        text: String,
    },
}
impl EngineEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Accepted => "accepted",
            Self::Cancelled => "cancelled",
            Self::Partial { .. } => "partial",
            Self::Final { .. } => "final",
            Self::Status { .. } => "status",
            Self::Error { .. } => "error",
        }
    }
}
pub trait SpeechEngine: Send {
    fn name(&self) -> &str;
    fn send(&mut self, command: EngineCommand) -> Reply<'_, ()>;
    fn event(&mut self) -> Reply<'_, EngineEvent>;
    fn close(&mut self) -> Reply<'_, ()>;
}

pub async fn create(language: &str, mode: &str, output: Output) -> Result<Box<dyn SpeechEngine>> {
    Ok(Box::new(
        process::ProcessEngine::start(language, mode, output).await?,
    ))
}
