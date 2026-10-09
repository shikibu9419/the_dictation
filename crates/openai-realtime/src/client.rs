//! WebSocket client for manual push-to-talk turns and function calling.
use crate::{
    events::{ClientEvent, ServerEvent, ToolSpec},
    session::{INPUT_SAMPLE_RATE, SessionConfig},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

pub const DEFAULT_MODEL: &str = "gpt-realtime-2.1";
/// The server rejects commits of less than 100 ms of audio.
pub const MIN_COMMIT_SAMPLES: usize = INPUT_SAMPLE_RATE as usize / 10;

/// Where outgoing events go. Production sends them over the socket; tests
/// capture them.
trait Outbound: Send + Sync {
    fn send(&self, event: Value) -> Result<()>;
}
struct SocketOutbound(mpsc::UnboundedSender<Value>);
impl Outbound for SocketOutbound {
    fn send(&self, event: Value) -> Result<()> {
        self.0
            .send(event)
            .map_err(|_| anyhow::anyhow!("Realtime connection closed"))
    }
}

/// Handle for one Realtime session. Clone-free: wrap in `Arc` to share.
pub struct RealtimeClient {
    outbound: Box<dyn Outbound>,
    /// Samples appended since the last commit or clear.
    pending_samples: AtomicUsize,
}

impl RealtimeClient {
    /// Open `wss://api.openai.com/v1/realtime?model=…`, send `session.update`
    /// and return the client plus the stream of parsed server events. The
    /// stream ends when the socket closes.
    pub async fn connect(
        api_key: &str,
        model: &str,
        config: &SessionConfig,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ServerEvent>)> {
        // Several TLS providers are compiled in through other crates; pick one.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let url = format!("wss://api.openai.com/v1/realtime?model={model}");
        let mut request = url.into_client_request()?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {api_key}")
                .parse()
                .context("API key contains invalid header characters")?,
        );
        let (socket, _) = connect_async(request)
            .await
            .context("Connect to the OpenAI Realtime API")?;
        let (mut writer, mut reader) = socket.split();
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Value>();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = outbound_rx.recv().await {
                if writer
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = writer.close().await;
        });
        tokio::spawn(async move {
            while let Some(message) = reader.next().await {
                let text = match message {
                    Ok(Message::Text(text)) => text,
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                match serde_json::from_str::<ServerEvent>(&text) {
                    Ok(event) => {
                        if event_tx.send(event).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = event_tx.send(ServerEvent::Error {
                            error: crate::events::ApiError {
                                r#type: Some("client_parse_error".into()),
                                code: None,
                                message: format!(
                                    "{error}: {}",
                                    text.chars().take(200).collect::<String>()
                                ),
                                param: None,
                            },
                        });
                    }
                }
            }
        });
        let client = Self {
            outbound: Box::new(SocketOutbound(outbound_tx)),
            pending_samples: AtomicUsize::new(0),
        };
        client.update_session(config)?;
        Ok((client, event_rx))
    }

    fn send(&self, event: ClientEvent) -> Result<()> {
        self.outbound.send(serde_json::to_value(event)?)
    }
    pub fn update_session(&self, config: &SessionConfig) -> Result<()> {
        self.send(ClientEvent::SessionUpdate {
            session: config.to_session_json(),
        })
    }
    /// Append 24 kHz mono PCM16 to the input buffer.
    pub fn append_audio(&self, pcm: &[i16]) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.send(ClientEvent::InputAudioBufferAppend {
            audio: STANDARD.encode(bytes),
        })?;
        self.pending_samples.fetch_add(pcm.len(), Ordering::SeqCst);
        Ok(())
    }
    pub fn pending_samples(&self) -> usize {
        self.pending_samples.load(Ordering::SeqCst)
    }
    /// Commit the buffer and request a response. Returns `false`, after
    /// clearing the buffer instead, when less than 100 ms was appended.
    pub fn commit_and_respond(&self) -> Result<bool> {
        let pending = self.pending_samples.swap(0, Ordering::SeqCst);
        if pending < MIN_COMMIT_SAMPLES {
            self.send(ClientEvent::InputAudioBufferClear)?;
            return Ok(false);
        }
        self.send(ClientEvent::InputAudioBufferCommit)?;
        self.send(ClientEvent::ResponseCreate)?;
        Ok(true)
    }
    pub fn clear(&self) -> Result<()> {
        self.pending_samples.store(0, Ordering::SeqCst);
        self.send(ClientEvent::InputAudioBufferClear)
    }
    pub fn cancel_response(&self) -> Result<()> {
        self.send(ClientEvent::ResponseCancel)
    }
    /// Tell the model how much of its spoken answer was actually heard.
    pub fn truncate(&self, item_id: &str, audio_end_ms: u64) -> Result<()> {
        self.send(ClientEvent::ConversationItemTruncate {
            item_id: item_id.into(),
            content_index: 0,
            audio_end_ms,
        })
    }
    /// Return a function result. Call `respond()` once the response that
    /// issued the call has finished.
    pub fn tool_output(&self, call_id: &str, output: &str) -> Result<()> {
        self.send(ClientEvent::ConversationItemCreate {
            item: json!({"type": "function_call_output", "call_id": call_id, "output": output}),
        })
    }
    pub fn respond(&self) -> Result<()> {
        self.send(ClientEvent::ResponseCreate)
    }
}

/// Convenience for apps that read the key from the environment.
pub fn api_key_from_env() -> Result<String> {
    match std::env::var("OPENAI_API_KEY") {
        Ok(key) if !key.trim().is_empty() => Ok(key),
        _ => bail!("OPENAI_API_KEY is not set"),
    }
}

/// Builder for a function tool, kept here so apps need not depend on serde_json's macros.
pub fn function_tool(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Capture(Arc<Mutex<Vec<Value>>>);
    impl Outbound for Capture {
        fn send(&self, event: Value) -> Result<()> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }
    fn client() -> (RealtimeClient, Arc<Mutex<Vec<Value>>>) {
        let sent = Arc::new(Mutex::new(vec![]));
        (
            RealtimeClient {
                outbound: Box::new(Capture(sent.clone())),
                pending_samples: AtomicUsize::new(0),
            },
            sent,
        )
    }
    fn types(sent: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
        sent.lock()
            .unwrap()
            .iter()
            .map(|e| e["type"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn short_buffers_are_cleared_instead_of_committed() {
        let (client, sent) = client();
        client
            .append_audio(&vec![1i16; MIN_COMMIT_SAMPLES - 1])
            .unwrap();
        assert!(!client.commit_and_respond().unwrap());
        assert_eq!(
            types(&sent),
            vec!["input_audio_buffer.append", "input_audio_buffer.clear"]
        );
        assert_eq!(client.pending_samples(), 0);
    }

    #[test]
    fn long_enough_buffers_commit_then_request_a_response() {
        let (client, sent) = client();
        client.append_audio(&vec![0i16; 1200]).unwrap();
        client.append_audio(&vec![0i16; 1200]).unwrap();
        assert!(client.commit_and_respond().unwrap());
        assert_eq!(
            types(&sent),
            vec![
                "input_audio_buffer.append",
                "input_audio_buffer.append",
                "input_audio_buffer.commit",
                "response.create"
            ]
        );
    }

    #[test]
    fn audio_is_little_endian_base64() {
        let (client, sent) = client();
        client.append_audio(&[1, -2]).unwrap();
        let audio = sent.lock().unwrap()[0]["audio"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(STANDARD.decode(audio).unwrap(), vec![1, 0, 0xfe, 0xff]);
        client.append_audio(&[]).unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn tool_output_is_a_function_call_output_item() {
        let (client, sent) = client();
        client.tool_output("call_1", "{\"ok\":true}").unwrap();
        client.respond().unwrap();
        let events = sent.lock().unwrap();
        assert_eq!(events[0]["type"], "conversation.item.create");
        assert_eq!(events[0]["item"]["type"], "function_call_output");
        assert_eq!(events[0]["item"]["call_id"], "call_1");
        assert_eq!(events[1]["type"], "response.create");
    }
}
