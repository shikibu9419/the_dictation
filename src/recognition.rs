use crate::{
    adapters::{
        input::{self, AudioChunk as Part, InputAdapter, InputEvent},
        speech::{self, EngineCommand, EngineReply, SpeechEngine},
    },
    helper::{Helper, ProcessGroup},
    output::Output,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{mpsc, oneshot},
    task::JoinSet,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Options {
    pub address: String,
    pub language: String,
    pub command: String,
    pub verbose: bool,
}
pub fn emit(event: Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{event}");
    let _ = out.flush();
}
#[derive(Default)]
struct Lifecycle {
    collecting: Option<bool>,
    released: HashSet<String>,
    finished: HashSet<String>,
    closed: HashSet<String>,
}
impl Lifecycle {
    fn suppressed(&self, key: &str) -> bool {
        self.collecting == Some(false) || self.released.contains(key)
    }
    fn retire(&mut self, key: &str) {
        if self.finished.contains(key) && self.closed.contains(key) {
            self.released.remove(key);
            self.finished.remove(key);
            self.closed.remove(key);
        }
    }
}
enum Job {
    Audio(Part, Instant),
    Release(String),
    Flush(oneshot::Sender<()>),
    Checkpoint(Value),
}
struct Speech {
    engine: Box<dyn SpeechEngine>,
    mode: &'static str,
    key: Option<String>,
    rate: Option<u32>,
    segments: Vec<(f64, f64)>,
    samples: usize,
    first_input: Option<Instant>,
    first_result: bool,
    lifecycle: Arc<Mutex<Lifecycle>>,
    output: Output,
}
impl Speech {
    async fn start(
        mode: &'static str,
        config: &speech::EngineConfig,
        options: &Options,
        lifecycle: Arc<Mutex<Lifecycle>>,
        output: Output,
    ) -> Result<Self> {
        let engine = speech::create(config, &options.language, mode, output.clone()).await?;
        let mut speech = Self {
            engine,
            mode,
            key: None,
            rate: None,
            segments: vec![],
            samples: 0,
            first_input: None,
            first_result: false,
            lifecycle,
            output,
        };
        loop {
            let event = speech.engine.event().await?;
            let ready = event.kind() == "ready";
            speech.event(event)?;
            if ready {
                speech.output.debug(format!(
                    "[{}] {} ready: {}",
                    mode,
                    speech.engine.name(),
                    options.language
                ));
                break;
            }
        }
        Ok(speech)
    }
    fn event(&mut self, event: EngineReply) -> Result<()> {
        let event = serde_json::to_value(event)?;
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "error" {
            bail!("{} {}: {}", self.engine.name(), self.mode, event["text"]);
        }
        if kind == "status" {
            self.output.debug(format!(
                "[{}] {}",
                self.mode,
                event["text"].as_str().unwrap_or("")
            ));
            if event.get("consumed_samples").is_some() || event.get("peak_memory_bytes").is_some() {
                self.output
                    .debug(format!("[{}] engine metrics: {event}", self.mode));
            }
            if let (Some(start), Some(end)) = (
                event["segment_start"].as_f64(),
                event["segment_end"].as_f64(),
            ) {
                self.segments.push((start, end));
            }
        }
        if kind == "partial" || kind == "final" {
            let text = event["text"].as_str().unwrap_or("");
            if !text.is_empty() && !self.first_result {
                if let Some(start) = self.first_input {
                    self.output.debug(format!(
                        "[{}] latency first PCM -> first text: {:.3}s",
                        self.mode,
                        start.elapsed().as_secs_f64()
                    ));
                }
                self.first_result = true;
            }
            let suppressed = self.mode == "live"
                && self
                    .lifecycle
                    .lock()
                    .unwrap()
                    .suppressed(self.key.as_deref().unwrap_or(""));
            let label = if suppressed && kind == "partial" {
                "suppressed partial"
            } else {
                kind
            };
            self.output.debug(format!(
                "[{}] {label} recording={:?}: {text}",
                self.mode, self.key
            ));
            if !suppressed || kind == "final" {
                let mut result = json!({"type":"text","text":text,"final":kind=="final","recording":self.key,"mode":self.mode});
                if kind == "final" {
                    result["segments"] = json!(self.segments);
                    result["audio_seconds"] =
                        json!(self.samples as f64 / self.rate.unwrap_or(1) as f64);
                }
                if !text.is_empty() || (kind == "final" && self.mode == "batch") {
                    emit(result);
                }
            }
        }
        Ok(())
    }
    async fn command(&mut self, message: EngineCommand, ack: &str) -> Result<()> {
        let kind = match &message {
            EngineCommand::Audio { .. } => "audio",
            EngineCommand::Finish => "finish",
            EngineCommand::Cancel => "cancel",
        };
        let started = Instant::now();
        self.output
            .debug(format!("[{}] engine input: {kind}", self.mode));
        let timeout = if kind == "finish"
            && matches!(self.engine.name(), "Whisper large-v3" | "Qwen3-ASR MLX")
        {
            Duration::from_secs_f64(120. + self.samples as f64 / self.rate.unwrap_or(1) as f64 * 2.)
        } else {
            Duration::from_secs(30)
        };
        tokio::time::timeout(timeout, async {
            self.engine.send(message).await?;
            loop {
                let event = self.engine.event().await?;
                let accepted = event.kind() == ack;
                self.event(event)?;
                if accepted {
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .with_context(|| {
            format!(
                "Speech engine {} stalled on {kind} (recording={:?})",
                self.mode, self.key
            )
        })??;
        self.output.debug(format!(
            "[{}] engine {kind} acknowledgement: {:.3}s",
            self.mode,
            started.elapsed().as_secs_f64()
        ));
        Ok(())
    }
    async fn feed(&mut self, part: Part) -> Result<()> {
        if self.key.as_ref().is_some_and(|key| *key != part.key) {
            self.cancel().await?;
        }
        self.key = Some(part.key.clone());
        if self.rate.is_none() {
            self.rate = Some(part.rate);
            self.samples = 0;
            self.segments.clear();
            self.first_input = None;
            self.first_result = false;
        }
        ensure!(
            self.rate == Some(part.rate),
            "Sample rate changed within recording"
        );
        let pcm = part.samples;
        self.samples += pcm.len();
        for block in pcm.chunks((part.rate / 2).max(1) as usize) {
            if self.first_input.is_none() {
                self.first_input = Some(Instant::now());
            }
            self.command(
                EngineCommand::Audio {
                    samples: block.to_vec(),
                    rate: part.rate,
                },
                "accepted",
            )
            .await?;
        }
        if part.final_part {
            self.command(EngineCommand::Finish, "final").await?;
            self.key = None;
            self.rate = None;
        }
        Ok(())
    }
    async fn cancel(&mut self) -> Result<()> {
        if self.key.is_some() {
            self.command(EngineCommand::Cancel, "cancelled").await?;
        }
        self.key = None;
        self.rate = None;
        Ok(())
    }
    async fn work(
        mut self,
        mut jobs: mpsc::UnboundedReceiver<Job>,
        input: Arc<Mutex<Box<dyn InputAdapter>>>,
    ) -> Result<()> {
        loop {
            tokio::select! {
                event=self.engine.event()=>{self.event(event?)?;}
                job=jobs.recv()=>{
                    match job {
                        Some(Job::Audio(part,queued))=>{
                            let key=part.key.clone();let checkpoint=part.checkpoint.clone();
                            if self.mode=="live" && self.lifecycle.lock().unwrap().suppressed(&key) {continue;}
                            self.output.debug(format!("latency {} input wait recording={key}: {:.3}s",self.mode,queued.elapsed().as_secs_f64()));
                            if self.mode=="batch" && part.final_part && part.samples.len() * 1000 < part.rate as usize * 150 {
                                self.output.debug(format!("recording={key}: below 150ms; returning empty text without speech inference"));
                                emit(json!({"type":"text","recording":key,"mode":"batch","final":true,"text":""}));
                            } else {
                                self.feed(part).await?;
                            }
                            if self.mode=="batch" {
                                if let Some(checkpoint) = checkpoint {input.lock().unwrap().commit(&checkpoint)?;}
                                input.lock().unwrap().completed(&key);
                                let mut life=self.lifecycle.lock().unwrap();life.finished.insert(key.clone());life.retire(&key);
                            }
                        }
                        Some(Job::Release(key))=>{
                            if self.key.as_ref()==Some(&key) {self.cancel().await?;}
                            let mut life=self.lifecycle.lock().unwrap();life.closed.insert(key.clone());life.retire(&key);
                        }
                        Some(Job::Checkpoint(value))=>{input.lock().unwrap().commit(&value)?;}
                        Some(Job::Flush(done))=>{let _=done.send(());}
                        None=>{self.engine.close().await?;return Ok(());}
                    }
                }
            }
        }
    }
}

struct Recognition {
    live: Option<mpsc::UnboundedSender<Job>>,
    batch: mpsc::UnboundedSender<Job>,
    lifecycle: Arc<Mutex<Lifecycle>>,
    audio: HashMap<String, (Vec<i16>, u32)>,
    output: Output,
}
impl Recognition {
    fn release(&self, key: &str) -> Result<()> {
        let newly_released = self
            .lifecycle
            .lock()
            .unwrap()
            .released
            .insert(key.to_owned());
        if newly_released {
            if let Some(live) = &self.live {
                live.send(Job::Release(key.to_owned()))?;
            } else {
                self.lifecycle.lock().unwrap().closed.insert(key.to_owned());
            }
            self.output.debug(format!(
                "live recognition stopped for recording={key}; receiving remaining audio"
            ));
        }
        Ok(())
    }
    fn state(&mut self, collecting: bool) -> Result<()> {
        emit(json!({"type":"state","collecting":collecting}));
        let previous = {
            let mut life = self.lifecycle.lock().unwrap();
            let p = life.collecting;
            life.collecting = Some(collecting);
            p
        };
        self.output.debug(format!(
            "input recording state {previous:?} -> {collecting}; active recordings={:?}",
            self.audio.keys()
        ));
        if previous == Some(true) && !collecting {
            for key in self.audio.keys() {
                self.release(key)?;
            }
        }
        Ok(())
    }
    fn add(&mut self, part: Part) -> Result<()> {
        let key = part.key.clone();
        if self.live.is_none()
            && !part.final_part
            && !part.samples.is_empty()
            && !self.lifecycle.lock().unwrap().suppressed(&key)
        {
            emit(json!({"type":"audio_level", "recording":key,
                "level":crate::audio_level::normalized(&part.samples)}));
        }
        if !self.audio.contains_key(&key) {
            let empty = part.final_part && part.samples.is_empty();
            emit(json!({"type":"recording","recording":key,"empty":empty}));
        }
        let recording = self
            .audio
            .entry(key.clone())
            .or_insert_with(|| (vec![], part.rate));
        ensure!(
            recording.1 == part.rate,
            "Sample rate changed within recording"
        );
        recording.0.extend_from_slice(&part.samples);
        if part.final_part {
            emit(json!({"type":"finalizing","recording":key}));
            self.release(&key)?;
            let (whole, rate) = self.audio.remove(&key).unwrap();
            self.output.debug(format!(
                "complete recording={key} queued for fresh recognition; duration={:.3}s",
                whole.len() as f64 / rate as f64
            ));
            self.batch.send(Job::Audio(
                Part {
                    key,
                    samples: whole,
                    rate,
                    final_part: true,
                    checkpoint: part.checkpoint,
                },
                Instant::now(),
            ))?;
        } else if !part.samples.is_empty()
            && !self.lifecycle.lock().unwrap().suppressed(&key)
            && let Some(live) = &self.live
        {
            live.send(Job::Audio(part, Instant::now()))?;
        }
        Ok(())
    }
    fn discard(&mut self, key: &str) -> Result<()> {
        emit(
            json!({"type":"discarded","recording":key,"text":"録音データの一部が入力元から失われました。もう一度録音してください。"}),
        );
        self.release(key)?;
        self.audio.remove(key);
        let mut life = self.lifecycle.lock().unwrap();
        life.finished.insert(key.into());
        life.retire(key);
        Ok(())
    }
    async fn input(mut self, input: Arc<Mutex<Box<dyn InputAdapter>>>) -> Result<()> {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut clock = tokio::time::interval(Duration::from_millis(50));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let events = tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break };
                    let message: Value = serde_json::from_str(&line)?;
                    input.lock().unwrap().decode(message, &self.output)?
                }
                _ = clock.tick() => input.lock().unwrap().poll(&self.output)?,
            };
            for event in events {
                match event {
                    InputEvent::Gesture(event) => {
                        emit(json!({"type":"gesture", "gesture":event.gesture,
                        "first_collection":event.first_collection, "last_collection":event.last_collection}))
                    }
                    InputEvent::Checkpoint(value) => self.batch.send(Job::Checkpoint(value))?,
                    InputEvent::Audio(part) => self.add(part)?,
                    InputEvent::Interrupted(key) => {
                        self.release(&key)?;
                        emit(json!({"type":"interrupted", "recording":key}));
                    }
                    InputEvent::Cancel(key) => {
                        self.release(&key)?;
                        self.audio.remove(&key);
                        let mut life = self.lifecycle.lock().unwrap();
                        life.finished.insert(key.clone());
                        life.retire(&key);
                        self.output.debug(format!("Recording cancelled key={key}; disconnect timeout=5s; no batch recognition"));
                        emit(json!({"type":"cancelled", "recording":key}));
                    }
                    InputEvent::Activity { key, collecting } => {
                        self.lifecycle.lock().unwrap().collecting = Some(collecting);
                        self.output.debug(format!(
                            "Recording activity key={key} collecting={collecting}"
                        ));
                        emit(json!({"type":"activity", "recording":key, "collecting":collecting}));
                    }
                    InputEvent::State(collecting) => self.state(collecting)?,
                    InputEvent::Discard(key) => self.discard(&key)?,
                    InputEvent::Flush => {
                        ensure!(
                            self.audio.is_empty(),
                            "Input ended before final audio for recordings: {:?}",
                            self.audio.keys().collect::<Vec<_>>()
                        );
                        let (live, live_done) = oneshot::channel();
                        let (batch, batch_done) = oneshot::channel();
                        if let Some(sender) = &self.live {
                            sender.send(Job::Flush(live))?;
                        } else {
                            let _ = live.send(());
                        }
                        self.batch.send(Job::Flush(batch))?;
                        live_done.await?;
                        batch_done.await?;
                        emit(json!({"type":"flushed"}));
                    }
                }
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    }
}

pub async fn worker(options: Options) -> Result<()> {
    let output = Output::new(options.verbose, None)?;
    let life = Arc::new(Mutex::new(Lifecycle::default()));
    let settings = crate::settings::Settings::load()?;
    if std::env::var_os("INDEX_VOICE_SPEECH_COMMAND").is_none() {
        settings.validate()?;
    }
    use crate::settings::SpeechModel;
    let config = |model| match model {
        SpeechModel::Apple => speech::EngineConfig::Apple,
        SpeechModel::OnDevice => speech::EngineConfig::Qwen {
            root: crate::settings::Settings::qwen_dir(),
        },
        SpeechModel::WhisperLargeV3 => speech::EngineConfig::Whisper {
            model: settings.model_path(),
        },
    };
    let plan = settings.recognition_plan();
    let live = if let Some(model) = plan.live {
        Some(
            Speech::start(
                "live",
                &config(model),
                &options,
                life.clone(),
                output.clone(),
            )
            .await?,
        )
    } else {
        output.debug("Live recognition disabled; recording goes directly to batch recognition");
        None
    };
    let batch = Speech::start(
        "batch",
        &config(plan.batch),
        &options,
        life.clone(),
        output.clone(),
    )
    .await?;
    let (ltx, lrx) = mpsc::unbounded_channel();
    let (btx, brx) = mpsc::unbounded_channel();
    let recognition = Recognition {
        live: live.as_ref().map(|_| ltx),
        batch: btx,
        lifecycle: life,
        audio: HashMap::new(),
        output,
    };
    let input = Arc::new(Mutex::new(input::create(
        &options.address,
        &options.command,
    )?));
    let mut tasks = JoinSet::new();
    if let Some(live) = live {
        tasks.spawn(live.work(lrx, input.clone()));
    }
    tasks.spawn(batch.work(brx, input.clone()));
    tasks.spawn(recognition.input(input));
    emit(json!({"type":"ready"}));
    let result = tasks.join_next().await.context("No recognition tasks")??;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}

pub struct Client {
    pub outbound: mpsc::UnboundedSender<Value>,
    pub events: mpsc::UnboundedReceiver<Result<Value>>,
    task: tokio::task::JoinHandle<()>,
    group: ProcessGroup,
}
impl Client {
    pub async fn start(options: Options, output: Output) -> Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("__worker")
            .arg(serde_json::to_string(&options)?)
            .process_group(0);
        let mut helper = Helper::spawn(command, output.clone(), String::new()).await?;
        let group = ProcessGroup(helper.child.id().context("Missing worker PID")? as i32);
        loop {
            let event = helper.event().await?;
            if event["type"] == "error" {
                bail!("Recognition: {}", event["text"]);
            }
            if event["type"] == "ready" {
                break;
            }
        }
        output.debug(format!(
            "recognition process ready pid={}; BLE pid={}",
            group.0,
            std::process::id()
        ));
        let (outbound, mut rx) = mpsc::unbounded_channel();
        let (tx, events) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    message=rx.recv()=>{
                        let Some(message)=message else {break};
                        if let Err(e)=helper.send(&message).await {let _=tx.send(Err(e));break;}
                    }
                    event=helper.event()=>{let failed=event.is_err();if tx.send(event).is_err()||failed {break;}}
                }
            }
            helper.close().await;
        });
        Ok(Self {
            outbound,
            events,
            task,
            group,
        })
    }
    pub fn send(&self, event: Value) -> Result<()> {
        self.outbound
            .send(event)
            .context("Recognition worker closed")
    }
    pub async fn flush(&mut self, output: &Output) -> Result<()> {
        self.send(json!({"type":"flush"}))?;
        loop {
            let event = self
                .events
                .recv()
                .await
                .context("Recognition worker closed")??;
            if event["type"] == "flushed" {
                return Ok(());
            }
            display_event(event, output)?;
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.task.abort();
        unsafe {
            libc::kill(-self.group.0, libc::SIGTERM);
        }
    }
}
pub fn display_event(event: Value, output: &Output) -> Result<()> {
    output.event(&event);
    if event["type"] == "error" {
        bail!("Recognition: {}", event["text"]);
    }
    if event["type"] == "text" {
        output.transcript(
            event["text"].as_str().unwrap_or(""),
            event["final"].as_bool().unwrap_or(false),
        );
    }
    Ok(())
}

#[cfg(test)]
mod gui_tests {
    use super::*;
    fn recognition() -> (
        Recognition,
        mpsc::UnboundedReceiver<Job>,
        mpsc::UnboundedReceiver<Job>,
    ) {
        let (live, lrx) = mpsc::unbounded_channel();
        let (batch, brx) = mpsc::unbounded_channel();
        (
            Recognition {
                live: Some(live),
                batch,
                lifecycle: Arc::new(Mutex::new(Lifecycle::default())),
                audio: HashMap::new(),
                output: Output::new(false, None).unwrap(),
            },
            lrx,
            brx,
        )
    }
    fn part(key: &str, samples: &[i16], final_part: bool) -> Part {
        Part {
            key: key.into(),
            samples: samples.to_vec(),
            rate: 9997,
            final_part,
            checkpoint: Some(json!(12)),
        }
    }
    #[test]
    fn release_stops_live_but_batch_receives_all_chunks() {
        let (mut r, mut live, mut batch) = recognition();
        r.state(true).unwrap();
        r.add(part("one", &[1, 2], false)).unwrap();
        assert!(matches!(live.try_recv().unwrap(), Job::Audio(_, _)));
        r.state(false).unwrap();
        assert!(matches!(live.try_recv().unwrap(), Job::Release(k) if k == "one"));
        r.add(part("one", &[3, 4], false)).unwrap();
        assert!(live.try_recv().is_err());
        r.add(part("one", &[5], true)).unwrap();
        match batch.try_recv().unwrap() {
            Job::Audio(p, _) => {
                assert_eq!(p.samples, [1, 2, 3, 4, 5]);
                assert!(p.final_part);
            }
            _ => panic!("expected complete batch"),
        }
        assert!(r.audio.is_empty());
    }
    #[test]
    fn second_recording_live_does_not_wait_for_first_batch() {
        let (mut r, mut live, mut batch) = recognition();
        r.state(true).unwrap();
        r.add(part("one", &[1], false)).unwrap();
        r.state(false).unwrap();
        r.add(part("one", &[2], true)).unwrap();
        r.state(true).unwrap();
        r.add(part("two", &[3], false)).unwrap();
        assert!(matches!(batch.try_recv().unwrap(), Job::Audio(p, _) if p.key == "one"));
        assert!(matches!(live.try_recv().unwrap(), Job::Audio(p, _) if p.key == "one"));
        assert!(matches!(live.try_recv().unwrap(), Job::Release(k) if k == "one"));
        assert!(matches!(live.try_recv().unwrap(), Job::Audio(p, _) if p.key == "two"));
        assert!(r.lifecycle.lock().unwrap().suppressed("one"));
        assert!(!r.lifecycle.lock().unwrap().suppressed("two"));
    }
    #[test]
    fn gui_receives_recording_identity_and_full_text_without_plain_text() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ipc.log");
        let mut output = Output::new(false, Some(&path)).unwrap();
        output.events = true;
        let event = json!({"type":"text","recording":"one","mode":"batch","final":true,"text":"長い録音の全文"});
        display_event(event.clone(), &output).unwrap();
        let log = std::fs::read_to_string(path).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert_eq!(serde_json::from_str::<Value>(log.trim()).unwrap(), event);
    }
}
