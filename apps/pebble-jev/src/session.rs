//! Push-to-talk session: ring audio in, Realtime API events out. Runs on the
//! tokio thread and reports to the GPUI view through `UiEvent`s.
use crate::{playback::Playback, resample::Resampler, settings::Settings, tools::Tools};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use openai_realtime::{
    INPUT_SAMPLE_RATE, OUTPUT_SAMPLE_RATE, RealtimeClient, ServerEvent, SessionConfig,
    Transcription,
};
use pebble_core::{config, output::Output, pcm::Pcm};
use pebble_ring::{
    capture::CaptureOptions,
    input::{RingInputConfig, gesture_types::Gesture},
    ring_input::{RingEvent, RingInput},
};
use std::{collections::HashMap, time::Duration};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    User {
        id: String,
        text: String,
        done: bool,
    },
    Assistant {
        id: String,
        text: String,
        done: bool,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: String,
        result: Option<String>,
        failed: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    Status(String),
    Recording(u64),
    Level { session: u64, level: f64 },
    Thinking,
    Speaking,
    Idle,
    Discarded,
    Conversation(Vec<Entry>),
    Error(String),
}
pub type UiSink = Box<dyn Fn(UiEvent) + Send + Sync>;

/// Ordered transcript of the current conversation, keyed by item ids.
#[derive(Default, Debug)]
pub struct Conversation {
    entries: Vec<Entry>,
}
impl Conversation {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub fn committed(&mut self, item_id: &str) {
        if !self
            .entries
            .iter()
            .any(|e| matches!(e, Entry::User { id, .. } if id == item_id))
        {
            self.entries.push(Entry::User {
                id: item_id.into(),
                text: String::new(),
                done: false,
            });
        }
    }
    pub fn user_delta(&mut self, item_id: &str, delta: &str) {
        self.committed(item_id);
        if let Some(Entry::User { text, .. }) = self.find_user(item_id) {
            text.push_str(delta);
        }
    }
    pub fn user_done(&mut self, item_id: &str, transcript: &str) {
        self.committed(item_id);
        if let Some(Entry::User { text, done, .. }) = self.find_user(item_id) {
            *text = transcript.trim().to_owned();
            *done = true;
        }
    }
    fn find_user(&mut self, item_id: &str) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| matches!(e, Entry::User { id, .. } if id == item_id))
    }
    fn assistant(&mut self, item_id: &str) -> &mut Entry {
        let index = self
            .entries
            .iter()
            .position(|e| matches!(e, Entry::Assistant { id, .. } if id == item_id))
            .unwrap_or_else(|| {
                self.entries.push(Entry::Assistant {
                    id: item_id.into(),
                    text: String::new(),
                    done: false,
                });
                self.entries.len() - 1
            });
        &mut self.entries[index]
    }
    pub fn assistant_delta(&mut self, item_id: &str, delta: &str) {
        if let Entry::Assistant { text, .. } = self.assistant(item_id) {
            text.push_str(delta);
        }
    }
    pub fn assistant_done(&mut self, item_id: &str, transcript: &str) {
        if let Entry::Assistant { text, done, .. } = self.assistant(item_id) {
            *text = transcript.trim().to_owned();
            *done = true;
        }
    }
    pub fn tool_call(&mut self, call_id: &str, name: &str, arguments: &str) {
        match self.find_call(call_id) {
            Some(Entry::ToolCall {
                name: existing,
                arguments: args,
                ..
            }) => {
                *existing = name.into();
                *args = arguments.into();
            }
            _ => self.entries.push(Entry::ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                arguments: arguments.into(),
                result: None,
                failed: false,
            }),
        }
    }
    pub fn tool_result(&mut self, call_id: &str, output: &str, failed: bool) {
        if let Some(Entry::ToolCall {
            result,
            failed: flag,
            ..
        }) = self.find_call(call_id)
        {
            *result = Some(output.into());
            *flag = failed;
        }
    }
    fn find_call(&mut self, call_id: &str) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| matches!(e, Entry::ToolCall { call_id: id, .. } if id == call_id))
    }
}

/// Tracks how much of each ring session was already streamed live, so the
/// final recording only sends the remainder.
#[derive(Default, Debug)]
pub struct PushToTalk {
    sent: HashMap<(u64, u64), usize>,
}
impl PushToTalk {
    pub fn live(&mut self, session: u64, generation: u64, samples: usize) {
        *self.sent.entry((session, generation)).or_default() += samples;
    }
    pub fn reset(&mut self, session: u64) {
        self.sent.retain(|(s, _), _| *s != session);
    }
    /// Start offset into the complete recording, or `None` when the server
    /// buffer holds audio from another generation and must be cleared first.
    pub fn remainder(&mut self, session: u64, generation: u64, total: usize) -> Option<usize> {
        let other_generation = self
            .sent
            .keys()
            .any(|(s, g)| *s == session && *g != generation);
        let sent = self.sent.remove(&(session, generation)).unwrap_or(0);
        self.reset(session);
        if other_generation || sent > total {
            None
        } else {
            Some(sent)
        }
    }
}

pub struct SessionOptions {
    pub settings: Settings,
    pub verbose: bool,
    pub ui: UiSink,
    pub stop: oneshot::Receiver<()>,
}

/// Run until `stop` fires. Reconnects to the API with backoff; ring input
/// errors end the session.
pub async fn run(options: SessionOptions) -> Result<()> {
    let SessionOptions {
        settings,
        verbose,
        ui,
        mut stop,
    } = options;
    let output = Output::new(verbose, None)?;
    let api_key = settings.api_key()?;
    let _lock = config::BluetoothLock::acquire()?;
    let address =
        config::load_address().context("リングが未登録です。pebble-jev pair を実行してください")?;
    let (ring, mut ring_rx) = RingInput::start(
        CaptureOptions {
            address,
            timeout: 30.,
            interval: settings.reception.state_poll_interval_ms as f64 / 1000.,
            pair: false,
            fetch: false,
        },
        RingInputConfig {
            reception: settings.reception,
            double_tap_enabled: false,
            live_mode: true,
        },
        output.clone(),
    )
    .await?;
    let playback = Playback::start(output.clone()).await?;
    let tools = Tools::new(Settings::directory());
    let session_config = SessionConfig {
        instructions: settings.instructions.clone(),
        voice: settings.voice.clone(),
        transcription: Some(Transcription {
            model: "gpt-4o-mini-transcribe".into(),
            language: Some(settings.language.clone()),
        }),
        tools: crate::tools::specs(),
        ..SessionConfig::default()
    };
    let mut backoff = Duration::from_secs(1);
    loop {
        ui(UiEvent::Status("接続中…".into()));
        let connected = tokio::select! {
            result = RealtimeClient::connect(&api_key, &settings.model, &session_config) => result,
            _ = &mut stop => return Ok(()),
        };
        match connected {
            Ok((client, events)) => {
                backoff = Duration::from_secs(1);
                ui(UiEvent::Status("リング待機中".into()));
                let mut talk = Talk {
                    client,
                    events,
                    ring: &ring,
                    playback: &playback,
                    tools: &tools,
                    ui: &ui,
                    output: &output,
                    conversation: Conversation::default(),
                    ptt: PushToTalk::default(),
                    resampler: Resampler::new(9997, INPUT_SAMPLE_RATE),
                    input_rate: 9997,
                    recording: None,
                    response_active: false,
                    speaking_item: None,
                    tool_output_pending: false,
                };
                tokio::select! {
                    result = talk.run(&mut ring_rx) => result?,
                    _ = &mut stop => return Ok(()),
                }
                output.error("Realtime connection closed; reconnecting");
            }
            Err(error) => {
                ui(UiEvent::Error(format!("{error:#}")));
                output.error(format!("Realtime connect: {error:#}"));
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = &mut stop => return Ok(()),
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

struct Talk<'a> {
    client: RealtimeClient,
    events: mpsc::UnboundedReceiver<ServerEvent>,
    ring: &'a RingInput,
    playback: &'a Playback,
    tools: &'a Tools,
    ui: &'a UiSink,
    output: &'a Output,
    conversation: Conversation,
    ptt: PushToTalk,
    resampler: Resampler,
    input_rate: u32,
    recording: Option<u64>,
    response_active: bool,
    speaking_item: Option<String>,
    tool_output_pending: bool,
}
impl Talk<'_> {
    /// Returns `Ok` when the socket closes (reconnect), `Err` when ring input ends.
    async fn run(&mut self, ring_rx: &mut mpsc::UnboundedReceiver<RingEvent>) -> Result<()> {
        loop {
            tokio::select! {
                event = ring_rx.recv() => match event {
                    Some(event) => self.ring_event(event).await?,
                    None => bail!("Ring input ended"),
                },
                event = self.events.recv() => match event {
                    Some(event) => self.server_event(event).await?,
                    None => return Ok(()),
                },
            }
        }
    }
    fn publish(&self) {
        (self.ui)(UiEvent::Conversation(self.conversation.entries().to_vec()));
    }
    async fn interrupt(&mut self) -> Result<()> {
        if self.response_active {
            self.client.cancel_response()?;
            self.response_active = false;
        }
        if let Some(item) = self.speaking_item.take() {
            let played = self.playback.played_ms();
            self.playback.clear().await?;
            if played > 0 {
                self.client.truncate(&item, played)?;
            }
        }
        Ok(())
    }
    fn resample(&mut self, pcm: &Pcm, rate: u32) -> Vec<i16> {
        if rate != self.input_rate {
            self.input_rate = rate;
            self.resampler = Resampler::new(rate, INPUT_SAMPLE_RATE);
        }
        let mut out = Vec::new();
        for block in pcm.slices() {
            out.extend(self.resampler.process(block));
        }
        out
    }
    async fn ring_event(&mut self, event: RingEvent) -> Result<()> {
        match event {
            RingEvent::PressStarted { session } => {
                self.interrupt().await?;
                self.client.clear()?;
                self.resampler.reset();
                self.ptt.reset(session);
                self.recording = Some(session);
                (self.ui)(UiEvent::Recording(session));
            }
            RingEvent::PressEnded { .. } => {}
            RingEvent::Audio {
                session,
                generation,
                pcm,
                rate,
            } => {
                let out = self.resample(&pcm, rate);
                self.client.append_audio(&out)?;
                self.ptt.live(session, generation, pcm.len());
            }
            RingEvent::LiveReset { session, .. } => {
                self.client.clear()?;
                self.resampler.reset();
                self.ptt.reset(session);
            }
            RingEvent::LiveStopped { .. } => {}
            RingEvent::Recording {
                session,
                generation,
                pcm,
                rate,
            } => {
                let start = match self.ptt.remainder(session, generation, pcm.len()) {
                    Some(start) => start,
                    None => {
                        self.client.clear()?;
                        self.resampler.reset();
                        0
                    }
                };
                let mut out = self.resample(&pcm.range(start..pcm.len()), rate);
                out.extend(self.resampler.flush());
                self.client.append_audio(&out)?;
                self.output.debug(format!(
                    "Recording session={session} samples={} sent_live={start} appended={}",
                    pcm.len(),
                    out.len()
                ));
                let responded = self.client.commit_and_respond()?;
                self.ring.completed(session);
                self.recording = None;
                if responded {
                    self.response_active = true;
                    (self.ui)(UiEvent::Thinking);
                } else {
                    (self.ui)(UiEvent::Discarded);
                    (self.ui)(UiEvent::Idle);
                }
            }
            RingEvent::Gesture(gesture) => {
                // A tap streamed nothing worth keeping: drop it, and stop any answer.
                self.client.clear()?;
                self.resampler.reset();
                if gesture.gesture == Gesture::SingleTap {
                    self.interrupt().await?;
                }
                self.recording = None;
                (self.ui)(UiEvent::Idle);
            }
            RingEvent::Level { session, level } => (self.ui)(UiEvent::Level { session, level }),
            RingEvent::Connected(connected) => (self.ui)(UiEvent::Status(
                if connected {
                    "リング接続中"
                } else {
                    "リングを探しています…"
                }
                .into(),
            )),
            RingEvent::Error(error) => {
                (self.ui)(UiEvent::Error(error.clone()));
                bail!("Ring input failed: {error}");
            }
        }
        Ok(())
    }
    async fn server_event(&mut self, event: ServerEvent) -> Result<()> {
        match event {
            ServerEvent::InputAudioBufferCommitted { item_id, .. } => {
                self.conversation.committed(&item_id);
                self.publish();
            }
            ServerEvent::InputAudioTranscriptionDelta { item_id, delta } => {
                self.conversation.user_delta(&item_id, &delta);
                self.publish();
            }
            ServerEvent::InputAudioTranscriptionCompleted {
                item_id,
                transcript,
            } => {
                self.conversation.user_done(&item_id, &transcript);
                self.publish();
            }
            ServerEvent::InputAudioTranscriptionFailed { item_id, error } => {
                self.conversation
                    .user_done(&item_id, &format!("(文字起こし失敗: {})", error.message));
                self.publish();
            }
            ServerEvent::ResponseCreated { .. } => {
                self.response_active = true;
                self.tool_output_pending = false;
            }
            ServerEvent::ResponseOutputItemAdded { item, .. } => {
                if item.r#type == "function_call" {
                    self.conversation.tool_call(
                        item.call_id.as_deref().unwrap_or(""),
                        item.name.as_deref().unwrap_or("?"),
                        "",
                    );
                    self.publish();
                }
            }
            ServerEvent::ResponseOutputAudioDelta { item_id, delta, .. } => {
                let bytes = STANDARD
                    .decode(delta)
                    .context("Audio delta is not base64")?;
                let pcm: Vec<i16> = bytes
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect();
                if self.speaking_item.as_deref() != Some(&item_id) {
                    self.speaking_item = Some(item_id);
                    (self.ui)(UiEvent::Speaking);
                }
                self.playback.push(&pcm, OUTPUT_SAMPLE_RATE).await?;
            }
            ServerEvent::ResponseOutputAudioTranscriptDelta { item_id, delta, .. }
            | ServerEvent::ResponseOutputTextDelta { item_id, delta, .. } => {
                self.conversation.assistant_delta(&item_id, &delta);
                self.publish();
            }
            ServerEvent::ResponseOutputAudioTranscriptDone {
                item_id,
                transcript,
                ..
            }
            | ServerEvent::ResponseOutputTextDone {
                item_id,
                text: transcript,
                ..
            } => {
                self.conversation.assistant_done(&item_id, &transcript);
                self.publish();
            }
            ServerEvent::ResponseFunctionCallArgumentsDone {
                call_id,
                name,
                arguments,
                ..
            } => {
                let name = name.unwrap_or_else(|| "?".into());
                self.conversation.tool_call(&call_id, &name, &arguments);
                let (result, failed) = match self.tools.execute(&name, &arguments) {
                    Ok(result) => (result, false),
                    Err(error) => (
                        serde_json::json!({"ok": false, "error": format!("{error:#}")}).to_string(),
                        true,
                    ),
                };
                self.conversation.tool_result(&call_id, &result, failed);
                self.publish();
                self.client.tool_output(&call_id, &result)?;
                self.tool_output_pending = true;
            }
            ServerEvent::ResponseDone { response } => {
                self.response_active = false;
                if response.status.as_deref() == Some("cancelled") {
                    self.tool_output_pending = false;
                }
                if self.tool_output_pending {
                    self.tool_output_pending = false;
                    self.client.respond()?;
                    self.response_active = true;
                    (self.ui)(UiEvent::Thinking);
                } else if self.recording.is_none() {
                    (self.ui)(UiEvent::Idle);
                }
            }
            ServerEvent::Error { error } => {
                self.output.error(format!(
                    "Realtime error: {} {:?}",
                    error.message, error.code
                ));
                if error.code.as_deref() != Some("input_audio_buffer_commit_empty") {
                    (self.ui)(UiEvent::Error(error.message));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remainder_skips_live_samples_of_the_same_generation() {
        let mut ptt = PushToTalk::default();
        ptt.live(1, 2, 2000);
        ptt.live(1, 2, 1500);
        assert_eq!(ptt.remainder(1, 2, 5000), Some(3500));
        assert_eq!(ptt.remainder(1, 2, 5000), Some(0), "state is cleared");
    }

    #[test]
    fn remainder_requires_a_clear_after_a_generation_change_or_overrun() {
        let mut ptt = PushToTalk::default();
        ptt.live(1, 1, 2000);
        assert_eq!(ptt.remainder(1, 2, 5000), None);
        let mut ptt = PushToTalk::default();
        ptt.live(1, 1, 6000);
        assert_eq!(ptt.remainder(1, 1, 5000), None);
        let mut ptt = PushToTalk::default();
        ptt.live(1, 1, 100);
        ptt.reset(1);
        assert_eq!(ptt.remainder(1, 1, 500), Some(0));
    }

    #[test]
    fn conversation_keeps_turn_order_and_streams_text() {
        let mut c = Conversation::default();
        c.committed("u1");
        c.user_delta("u1", "牛乳を");
        c.user_delta("u1", "買う");
        c.tool_call("call_1", "add_todo", "");
        c.tool_call("call_1", "add_todo", "{\"title\":\"牛乳\"}");
        c.tool_result("call_1", "{\"ok\":true}", false);
        c.assistant_delta("a1", "追加");
        c.assistant_done("a1", "追加しました ");
        c.user_done("u1", " 牛乳を買う ");
        let entries = c.entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            Entry::User {
                id: "u1".into(),
                text: "牛乳を買う".into(),
                done: true
            }
        );
        let Entry::ToolCall {
            arguments, result, ..
        } = &entries[1]
        else {
            panic!()
        };
        assert_eq!(arguments, "{\"title\":\"牛乳\"}");
        assert_eq!(result.as_deref(), Some("{\"ok\":true}"));
        assert_eq!(
            entries[2],
            Entry::Assistant {
                id: "a1".into(),
                text: "追加しました".into(),
                done: true
            }
        );
    }
}
