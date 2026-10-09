//! Minimal OpenAI Realtime API (GA) client: manual push-to-talk audio turns,
//! streamed transcripts and audio, and function calling over WebSocket.
pub mod client;
pub mod events;
pub mod session;

pub use client::{
    DEFAULT_MODEL, MIN_COMMIT_SAMPLES, RealtimeClient, api_key_from_env, function_tool,
};
pub use events::{ApiError, ClientEvent, Item, Response, ServerEvent, ToolSpec};
pub use session::{INPUT_SAMPLE_RATE, OUTPUT_SAMPLE_RATE, SessionConfig, Transcription};
