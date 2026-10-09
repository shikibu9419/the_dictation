//! `session.update` payload for a manual push-to-talk audio session.
use crate::events::ToolSpec;
use serde_json::{Value, json};

pub const INPUT_SAMPLE_RATE: u32 = 24_000;
pub const OUTPUT_SAMPLE_RATE: u32 = 24_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Transcription {
    pub model: String,
    pub language: Option<String>,
    /// Free text that steers the transcription model, e.g. expected languages.
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionConfig {
    pub instructions: String,
    pub voice: String,
    /// `["audio"]` yields audio plus a transcript; `["text"]` yields text only.
    pub output_modalities: Vec<String>,
    /// Transcribe the user's committed audio; without it no user transcript arrives.
    pub transcription: Option<Transcription>,
    pub tools: Vec<ToolSpec>,
    pub reasoning_effort: Option<String>,
}
impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            instructions: String::new(),
            voice: "marin".into(),
            output_modalities: vec!["audio".into()],
            transcription: Some(Transcription {
                model: "gpt-4o-mini-transcribe".into(),
                language: None,
                prompt: None,
            }),
            tools: vec![],
            reasoning_effort: Some("low".into()),
        }
    }
}
impl SessionConfig {
    /// The `session` object of a `session.update` event. Turn detection is off:
    /// the caller commits the buffer explicitly.
    pub fn to_session_json(&self) -> Value {
        let mut input = json!({
            "format": {"type": "audio/pcm", "rate": INPUT_SAMPLE_RATE},
            "turn_detection": null,
            "noise_reduction": {"type": "near_field"},
        });
        if let Some(transcription) = &self.transcription {
            let mut value = json!({"model": transcription.model});
            if let Some(language) = &transcription.language {
                value["language"] = json!(language);
            }
            if let Some(prompt) = &transcription.prompt {
                value["prompt"] = json!(prompt);
            }
            input["transcription"] = value;
        }
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                })
            })
            .collect();
        let mut session = json!({
            "type": "realtime",
            "instructions": self.instructions,
            "output_modalities": self.output_modalities,
            "audio": {
                "input": input,
                "output": {
                    "format": {"type": "audio/pcm", "rate": OUTPUT_SAMPLE_RATE},
                    "voice": self.voice,
                },
            },
            "tools": tools,
            "tool_choice": "auto",
        });
        if let Some(effort) = &self.reasoning_effort {
            session["reasoning"] = json!({"effort": effort});
        }
        session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_json_uses_ga_shape_and_manual_turns() {
        let config = SessionConfig {
            instructions: "Be brief.".into(),
            tools: vec![ToolSpec {
                name: "add_todo".into(),
                description: "Add a todo".into(),
                parameters: json!({"type": "object", "properties": {"title": {"type": "string"}}, "required": ["title"]}),
            }],
            transcription: Some(Transcription {
                model: "gpt-4o-mini-transcribe".into(),
                language: Some("ja".into()),
                prompt: Some("Japanese".into()),
            }),
            ..SessionConfig::default()
        };
        let session = config.to_session_json();
        assert_eq!(session["type"], "realtime");
        assert_eq!(session["audio"]["input"]["format"]["rate"], 24000);
        assert!(session["audio"]["input"]["turn_detection"].is_null());
        assert_eq!(session["audio"]["input"]["transcription"]["language"], "ja");
        assert_eq!(
            session["audio"]["input"]["transcription"]["prompt"],
            "Japanese"
        );
        assert_eq!(session["audio"]["output"]["voice"], "marin");
        assert_eq!(session["tools"][0]["type"], "function");
        assert_eq!(session["tools"][0]["name"], "add_todo");
        assert_eq!(session["reasoning"]["effort"], "low");
        assert!(session.get("model").is_none(), "model is fixed by the URL");
    }

    #[test]
    fn transcription_can_be_omitted() {
        let config = SessionConfig {
            transcription: None,
            reasoning_effort: None,
            ..SessionConfig::default()
        };
        let session = config.to_session_json();
        assert!(session["audio"]["input"].get("transcription").is_none());
        assert!(session.get("reasoning").is_none());
    }
}
