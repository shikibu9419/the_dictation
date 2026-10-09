mod native_session;
mod process;
pub mod run_control;
pub mod whisper;

use anyhow::Result;
use pebble_core::output::Output;
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
/// Preserve cursor/timing fields from native adapters while accepting legacy
/// adapters with no protocol metadata.
#[derive(Debug, Deserialize, Serialize)]
pub struct EngineReply {
    #[serde(flatten)]
    pub event: EngineEvent,
    #[serde(flatten)]
    pub metadata: EngineMetadata,
}
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct EngineMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_samples: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumed_samples: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_samples: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_window_samples: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_memory_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permitted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permit_request: Option<u64>,
}
impl EngineReply {
    pub fn kind(&self) -> &'static str {
        self.event.kind()
    }
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
    fn process_id(&self) -> Option<u32> {
        None
    }
    fn send(&mut self, command: EngineCommand) -> Reply<'_, ()>;
    fn event(&mut self) -> Reply<'_, EngineReply>;
    fn close(&mut self) -> Reply<'_, ()>;
    fn input_backlogged(&self) -> bool {
        false
    }
    fn control(&self) -> Option<std::sync::Arc<dyn run_control::ExecutionControl>> {
        None
    }
}

/// Resolved at the application boundary; adapters never read user settings.
#[derive(Clone, Debug)]
pub enum EngineConfig {
    Apple,
    Qwen { root: std::path::PathBuf },
    Whisper { model: std::path::PathBuf },
}

pub async fn create(
    config: &EngineConfig,
    language: &str,
    mode: &str,
    output: Output,
) -> Result<Box<dyn SpeechEngine>> {
    Ok(Box::new(
        process::ProcessEngine::start(config, language, mode, output).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn reply_preserves_native_cursors_and_legacy_messages() {
        let value = json!({"type":"status","text":"consumed","protocol_version":2,"session_id":7,"generation":8,"consumed_samples":16000,"segment_start":0.0,"segment_end":1.0});
        let reply: EngineReply = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(reply.kind(), "status");
        assert_eq!(serde_json::to_value(reply).unwrap(), value);
        let reply: EngineReply = serde_json::from_value(json!({"type":"ready"})).unwrap();
        assert_eq!(reply.kind(), "ready");
        assert_eq!(
            serde_json::to_value(reply).unwrap(),
            json!({"type":"ready"})
        );
    }
}
