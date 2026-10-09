//! The subset of GA Realtime API events this client sends and receives.
//! Unknown server events deserialize to `ServerEvent::Other` so new event
//! types never break the stream.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ApiError {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub param: Option<String>,
}

/// A conversation item as delivered inside `conversation.item.*` and
/// `response.output_item.*` events. Only the fields this client reads.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Item {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub call_id: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Response {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub output: Vec<Item>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "session.created")]
    SessionCreated { session: Value },
    #[serde(rename = "session.updated")]
    SessionUpdated { session: Value },
    #[serde(rename = "error")]
    Error { error: ApiError },
    #[serde(rename = "input_audio_buffer.committed")]
    InputAudioBufferCommitted {
        item_id: String,
        #[serde(default)]
        previous_item_id: Option<String>,
    },
    #[serde(rename = "input_audio_buffer.cleared")]
    InputAudioBufferCleared,
    #[serde(rename = "conversation.item.added")]
    ConversationItemAdded { item: Item },
    #[serde(rename = "conversation.item.done")]
    ConversationItemDone { item: Item },
    #[serde(rename = "conversation.item.input_audio_transcription.delta")]
    InputAudioTranscriptionDelta { item_id: String, delta: String },
    #[serde(rename = "conversation.item.input_audio_transcription.completed")]
    InputAudioTranscriptionCompleted { item_id: String, transcript: String },
    #[serde(rename = "conversation.item.input_audio_transcription.failed")]
    InputAudioTranscriptionFailed { item_id: String, error: ApiError },
    #[serde(rename = "response.created")]
    ResponseCreated { response: Response },
    #[serde(rename = "response.output_item.added")]
    ResponseOutputItemAdded { response_id: String, item: Item },
    #[serde(rename = "response.output_item.done")]
    ResponseOutputItemDone { response_id: String, item: Item },
    #[serde(rename = "response.output_audio.delta")]
    ResponseOutputAudioDelta {
        response_id: String,
        item_id: String,
        /// Base64 PCM16 little-endian mono at 24 kHz.
        delta: String,
    },
    #[serde(rename = "response.output_audio.done")]
    ResponseOutputAudioDone {
        response_id: String,
        item_id: String,
    },
    #[serde(rename = "response.output_audio_transcript.delta")]
    ResponseOutputAudioTranscriptDelta {
        response_id: String,
        item_id: String,
        delta: String,
    },
    #[serde(rename = "response.output_audio_transcript.done")]
    ResponseOutputAudioTranscriptDone {
        response_id: String,
        item_id: String,
        transcript: String,
    },
    #[serde(rename = "response.output_text.delta")]
    ResponseOutputTextDelta {
        response_id: String,
        item_id: String,
        delta: String,
    },
    #[serde(rename = "response.output_text.done")]
    ResponseOutputTextDone {
        response_id: String,
        item_id: String,
        text: String,
    },
    #[serde(rename = "response.function_call_arguments.delta")]
    ResponseFunctionCallArgumentsDelta {
        response_id: String,
        item_id: String,
        call_id: String,
        delta: String,
    },
    #[serde(rename = "response.function_call_arguments.done")]
    ResponseFunctionCallArgumentsDone {
        response_id: String,
        item_id: String,
        call_id: String,
        #[serde(default)]
        name: Option<String>,
        arguments: String,
    },
    #[serde(rename = "response.done")]
    ResponseDone { response: Response },
    #[serde(rename = "rate_limits.updated")]
    RateLimitsUpdated { rate_limits: Value },
    #[serde(other)]
    Other,
}

/// Function tool definition sent in `session.tools`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type")]
pub enum ClientEvent {
    #[serde(rename = "session.update")]
    SessionUpdate { session: Value },
    #[serde(rename = "input_audio_buffer.append")]
    InputAudioBufferAppend { audio: String },
    #[serde(rename = "input_audio_buffer.commit")]
    InputAudioBufferCommit,
    #[serde(rename = "input_audio_buffer.clear")]
    InputAudioBufferClear,
    #[serde(rename = "conversation.item.create")]
    ConversationItemCreate { item: Value },
    #[serde(rename = "conversation.item.truncate")]
    ConversationItemTruncate {
        item_id: String,
        content_index: u32,
        audio_end_ms: u64,
    },
    #[serde(rename = "response.create")]
    ResponseCreate,
    #[serde(rename = "response.cancel")]
    ResponseCancel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ga_audio_and_tool_events_deserialize() {
        let audio: ServerEvent = serde_json::from_value(json!({
            "type": "response.output_audio.delta", "event_id": "e1",
            "response_id": "r1", "item_id": "i1", "output_index": 0, "content_index": 0,
            "delta": "AAAA"
        }))
        .unwrap();
        assert_eq!(
            audio,
            ServerEvent::ResponseOutputAudioDelta {
                response_id: "r1".into(),
                item_id: "i1".into(),
                delta: "AAAA".into()
            }
        );
        let call: ServerEvent = serde_json::from_value(json!({
            "type": "response.function_call_arguments.done", "response_id": "r1",
            "item_id": "i2", "output_index": 1, "call_id": "call_1", "name": "add_todo",
            "arguments": "{\"title\":\"milk\"}"
        }))
        .unwrap();
        let ServerEvent::ResponseFunctionCallArgumentsDone {
            call_id, arguments, ..
        } = call
        else {
            panic!("function call expected")
        };
        assert_eq!(call_id, "call_1");
        assert_eq!(arguments, "{\"title\":\"milk\"}");
    }

    #[test]
    fn unknown_and_beta_event_names_are_ignored_not_errors() {
        for name in [
            "response.audio.delta",
            "output_audio_buffer.started",
            "mcp_list_tools.in_progress",
        ] {
            let event: ServerEvent = serde_json::from_value(json!({"type": name, "x": 1})).unwrap();
            assert_eq!(event, ServerEvent::Other, "{name}");
        }
    }

    #[test]
    fn response_done_carries_function_call_items() {
        let event: ServerEvent = serde_json::from_value(json!({
            "type": "response.done",
            "response": {"id": "r9", "status": "completed", "output": [
                {"id": "i1", "type": "function_call", "name": "add_memo", "call_id": "c1", "arguments": "{}"}
            ]}
        }))
        .unwrap();
        let ServerEvent::ResponseDone { response } = event else {
            panic!()
        };
        assert_eq!(response.status.as_deref(), Some("completed"));
        assert_eq!(response.output[0].call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn client_events_serialize_with_ga_type_tags() {
        let json = serde_json::to_value(ClientEvent::ConversationItemTruncate {
            item_id: "i1".into(),
            content_index: 0,
            audio_end_ms: 1500,
        })
        .unwrap();
        assert_eq!(json["type"], "conversation.item.truncate");
        assert_eq!(json["audio_end_ms"], 1500);
        assert_eq!(
            serde_json::to_value(ClientEvent::ResponseCreate).unwrap(),
            json!({"type": "response.create"})
        );
    }
}
