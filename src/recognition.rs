use crate::{
    adapters::{
        input::{self, AudioChunk as Part, InputAdapter, InputEvent},
        speech::{
            self, EngineCommand, EngineReply, SpeechEngine,
            run_control::{LivePriority, active_timeout},
        },
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
struct ReceptionControl {
    generation: u64,
    live: bool,
    visible: bool,
}
#[derive(Default)]
struct Lifecycle {
    collecting: Option<bool>,
    released: HashSet<String>,
    finished: HashSet<String>,
    closed: HashSet<String>,
    reception: HashMap<String, ReceptionControl>,
}
impl Lifecycle {
    fn suppressed(&self, key: &str) -> bool {
        self.reception.get(key).map_or(
            self.collecting == Some(false) || self.released.contains(key),
            |s| !s.live,
        )
    }
    fn permits(&self, key: &str, generation: Option<u64>) -> bool {
        generation.map_or_else(
            || !self.suppressed(key),
            |generation| {
                self.reception
                    .get(key)
                    .is_some_and(|s| s.live && s.generation == generation)
            },
        )
    }
    fn visible(&self, key: &str, generation: Option<u64>) -> bool {
        self.permits(key, generation)
            && generation.is_none_or(|_| self.reception.get(key).is_some_and(|s| s.visible))
    }
    fn retire(&mut self, key: &str) {
        if self.finished.contains(key) && self.closed.contains(key) {
            self.released.remove(key);
            self.finished.remove(key);
            self.closed.remove(key);
            self.reception.remove(key);
        }
    }
}
enum Job {
    Audio(Part, Instant),
    ReceptionAudio(Part, Instant, u64),
    Reset(String, u64),
    Release(String),
    Flush(oneshot::Sender<()>),
    Checkpoint(Value),
}
struct Speech {
    engine: Box<dyn SpeechEngine>,
    mode: &'static str,
    key: Option<String>,
    generation: Option<u64>,
    rate: Option<u32>,
    segments: Vec<(f64, f64)>,
    samples: usize,
    first_input: Option<Instant>,
    first_result: bool,
    lifecycle: Arc<Mutex<Lifecycle>>,
    output: Output,
    priority: Option<LivePriority>,
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
            generation: None,
            rate: None,
            segments: vec![],
            samples: 0,
            first_input: None,
            first_result: false,
            lifecycle,
            output,
            priority: None,
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
                && !self
                    .lifecycle
                    .lock()
                    .unwrap()
                    .visible(self.key.as_deref().unwrap_or(""), self.generation);
            let label = if suppressed && kind == "partial" {
                "suppressed partial"
            } else {
                kind
            };
            self.output.debug(format!(
                "[{}] {label} recording={:?}: {text}",
                self.mode, self.key
            ));
            if !suppressed {
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
        let activity = (kind == "finish")
            .then(|| self.engine.control())
            .flatten()
            .map(|control| control.activity());
        active_timeout(timeout, activity, async {
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
        })?;
        self.output.debug(format!(
            "[{}] engine {kind} acknowledgement: {:.3}s",
            self.mode,
            started.elapsed().as_secs_f64()
        ));
        Ok(())
    }
    async fn feed(&mut self, part: Part, queued: Instant, generation: Option<u64>) -> Result<()> {
        if self
            .key
            .as_ref()
            .is_some_and(|key| *key != part.key || self.generation != generation)
        {
            self.cancel().await?;
        }
        if self.key.is_none()
            && let Some(priority) = &mut self.priority
        {
            priority.acquire().await?;
        }
        self.key = Some(part.key.clone());
        self.generation = generation;
        if self.rate.is_none() {
            self.rate = Some(part.rate);
            self.samples = 0;
            self.segments.clear();
            self.first_input = (!part.samples.is_empty()).then_some(queued);
            self.first_result = false;
        }
        ensure!(
            self.rate == Some(part.rate),
            "Sample rate changed within recording"
        );
        let pcm = part.samples;
        self.samples += pcm.len();
        for block in pcm.chunks((part.rate / 2).max(1) as usize) {
            if self.mode == "live"
                && !self
                    .lifecycle
                    .lock()
                    .unwrap()
                    .permits(&part.key, generation)
            {
                self.cancel().await?;
                return Ok(());
            }
            while self.engine.input_backlogged() {
                let activity = self.engine.control().map(|c| c.activity());
                let event =
                    active_timeout(Duration::from_secs(120), activity, self.engine.event()).await?;
                self.event(event)?;
            }
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
            if let Some(priority) = &mut self.priority {
                priority.release().await?;
            }
        }
        Ok(())
    }
    async fn cancel(&mut self) -> Result<()> {
        if self.key.is_some() {
            self.command(EngineCommand::Cancel, "cancelled").await?;
        }
        self.key = None;
        self.generation = None;
        self.rate = None;
        if let Some(priority) = &mut self.priority {
            priority.release().await?;
        }
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
                        Some(job @ (Job::Audio(..) | Job::ReceptionAudio(..)))=>{
                            let (part, queued, generation) = match job {
                                Job::Audio(part, queued) => (part, queued, None),
                                Job::ReceptionAudio(part, queued, generation) => (part, queued, Some(generation)),
                                _ => unreachable!(),
                            };
                            let key=part.key.clone();let checkpoint=part.checkpoint.clone();
                            if self.mode=="live" && !self.lifecycle.lock().unwrap().permits(&key, generation) {continue;}
                            self.output.debug(format!("latency {} input wait recording={key}: {:.3}s",self.mode,queued.elapsed().as_secs_f64()));
                            self.feed(part,queued,generation).await?;
                            if self.mode=="batch" {
                                if let Some(checkpoint) = checkpoint {input.lock().unwrap().commit(&checkpoint)?;}
                                input.lock().unwrap().completed(&key);
                                let mut life=self.lifecycle.lock().unwrap();life.finished.insert(key.clone());life.retire(&key);
                            }
                        }
                        Some(Job::Release(key))=>{
                            if self.key.as_ref()==Some(&key) && (self.generation.is_none() || !self.lifecycle.lock().unwrap().permits(&key, self.generation)) {self.cancel().await?;}
                            let mut life=self.lifecycle.lock().unwrap();
                            if life.reception.contains_key(&key) || life.released.contains(&key) || life.finished.contains(&key) {
                                life.closed.insert(key.clone());life.retire(&key);
                            }
                        }
                        Some(Job::Reset(key,generation))=>{
                            if self.key.as_ref()==Some(&key) && self.generation != Some(generation) {self.cancel().await?;}
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
    audio: HashMap<String, (crate::pcm::Pcm, u32)>,
    output: Output,
}
impl Recognition {
    fn reception(
        &mut self,
        namespace: &str,
        effect: pebble_index::reception::input_effects::Effect,
    ) -> Result<()> {
        use pebble_index::reception::{input_effects::Effect, session_state::Action};
        let key = |id| format!("{namespace}-{id}");
        match effect {
            Effect::Snapshot(snapshot) => {
                emit(json!({"type":"reception_state", "namespace":namespace, "state":snapshot}))
            }
            Effect::View(view) => {
                let key = key(view.id);
                let was_visible = {
                    let mut life = self.lifecycle.lock().unwrap();
                    let control = life.reception.entry(key.clone()).or_default();
                    let previous = control.visible;
                    control.visible = view.visible;
                    control.live = view.live && !view.failed;
                    previous
                };
                if view.failed {
                    emit(
                        json!({"type":"discarded", "recording":key,"text":"録音データの一部が失われました。受信済み音声は保持しています。"}),
                    );
                } else if view.visible {
                    emit(
                        json!({"type":"reception_activity", "recording":key,"collecting":view.live}),
                    );
                } else if was_visible {
                    emit(json!({"type":"cancelled", "recording":key}));
                }
            }
            Effect::ResetLive {
                session,
                generation,
            } => {
                let key = key(session);
                self.lifecycle
                    .lock()
                    .unwrap()
                    .reception
                    .entry(key.clone())
                    .or_default()
                    .generation = generation;
                if let Some(live) = &self.live {
                    live.send(Job::Reset(key, generation))?;
                }
            }
            Effect::StopLive(session) => {
                let key = key(session);
                if let Some(control) = self.lifecycle.lock().unwrap().reception.get_mut(&key) {
                    control.live = false;
                }
                if let Some(live) = &self.live {
                    live.send(Job::Release(key))?;
                } else {
                    self.lifecycle.lock().unwrap().closed.insert(key);
                }
            }
            Effect::Live(plan) => {
                let key = key(plan.session);
                self.lifecycle
                    .lock()
                    .unwrap()
                    .reception
                    .entry(key.clone())
                    .or_default()
                    .generation = plan.generation;
                self.output.debug(format!(
                    "live PCM recording={key} generation={} samples={}..{}",
                    plan.generation,
                    plan.start_sample,
                    plan.start_sample + plan.pcm.len()
                ));
                if let Some(live) = &self.live {
                    live.send(Job::ReceptionAudio(
                        Part {
                            key,
                            samples: plan.pcm,
                            rate: plan.rate,
                            final_part: false,
                            checkpoint: None,
                        },
                        Instant::now(),
                        plan.generation,
                    ))?;
                }
            }
            Effect::Batch(plan) => {
                let key = key(plan.session);
                // A complete recording can arrive without a preceding S=true.
                // Create its result item directly in dictating, never recording.
                emit(json!({"type":"reception_activity", "recording":key,"collecting":false}));
                self.output.debug(format!(
                    "whole PCM recording={key} samples={} duration={:.3}s",
                    plan.pcm.len(),
                    plan.pcm.len() as f64 / plan.rate as f64
                ));
                self.batch.send(Job::Audio(
                    Part {
                        key,
                        samples: plan.pcm,
                        rate: plan.rate,
                        final_part: true,
                        checkpoint: None,
                    },
                    Instant::now(),
                ))?;
            }
            Effect::Action(Action::MergeSession { from, into }) => {
                let from = key(from);
                self.lifecycle.lock().unwrap().reception.remove(&from);
                emit(
                    json!({"type":"reception_merge","recording":from,"target_recording":key(into)}),
                );
            }
            Effect::Action(Action::Retire { session, .. }) => {
                let key = key(session);
                let mut life = self.lifecycle.lock().unwrap();
                let visible = life.reception.get(&key).is_some_and(|s| s.visible);
                if let Some(control) = life.reception.get_mut(&key) {
                    control.live = false;
                }
                // Batch completion sets finished before the input acknowledges it.
                if !life.finished.contains(&key) && visible {
                    emit(json!({"type":"cancelled","recording":key}));
                }
                life.reception.remove(&key);
                life.released.remove(&key);
                life.finished.remove(&key);
                life.closed.remove(&key);
            }
            Effect::Action(_) => {}
        }
        Ok(())
    }
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
                "level":crate::audio_level::normalized_iter(part.samples.iter().copied())}));
        }
        if !self.audio.contains_key(&key) {
            let empty = part.final_part && part.samples.is_empty();
            emit(json!({"type":"recording","recording":key,"empty":empty}));
        }
        let recording = self
            .audio
            .entry(key.clone())
            .or_insert_with(|| (Default::default(), part.rate));
        ensure!(
            recording.1 == part.rate,
            "Sample rate changed within recording"
        );
        recording.0.append(&part.samples);
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
        let mut clock = tokio::time::interval(Duration::from_millis(5));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let events = tokio::select! {
                biased;
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
                    InputEvent::Reception { namespace, effect } => {
                        self.reception(&namespace, effect)?
                    }
                    InputEvent::Level { key, level } => {
                        if self
                            .lifecycle
                            .lock()
                            .unwrap()
                            .reception
                            .get(&key)
                            .is_some_and(|s| s.visible && s.live)
                        {
                            emit(json!({"type":"audio_level", "recording":key, "level":level}));
                        }
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
    let mut live = if let Some(model) = plan.live {
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
    if let Some(live) = &mut live
        && let Some(batch_control) = batch.engine.control()
    {
        live.priority = Some(LivePriority::new(live.engine.control(), batch_control).await?);
    }
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
    #[test]
    fn reception_release_and_generation_are_per_recording_and_hidden_results_stay_hidden() {
        use pebble_index::reception::{input_effects::Effect, session_state::Action};
        let (mut r, _, _) = recognition();
        // Keep receivers alive for the queued reset/release commands.
        let (tx, _rx) = mpsc::unbounded_channel();
        r.live = Some(tx);
        {
            let mut life = r.lifecycle.lock().unwrap();
            life.collecting = Some(false); // Generic microphone state does not govern Index sessions.
            life.reception.insert(
                "ring-1".into(),
                ReceptionControl {
                    generation: 0,
                    live: true,
                    visible: true,
                },
            );
            life.reception.insert(
                "ring-2".into(),
                ReceptionControl {
                    generation: 0,
                    live: true,
                    visible: false,
                },
            );
        }
        r.reception("ring", Effect::StopLive(1)).unwrap();
        {
            let life = r.lifecycle.lock().unwrap();
            assert!(!life.permits("ring-1", Some(0)));
            assert!(life.permits("ring-2", Some(0)));
            assert!(!life.visible("ring-2", Some(0)));
        }
        r.reception(
            "ring",
            Effect::ResetLive {
                session: 2,
                generation: 1,
            },
        )
        .unwrap();
        {
            let life = r.lifecycle.lock().unwrap();
            assert!(!life.permits("ring-2", Some(0)));
            assert!(life.permits("ring-2", Some(1)));
        }
        r.reception(
            "ring",
            Effect::Action(Action::Retire {
                session: 2,
                sources: vec![],
            }),
        )
        .unwrap();
        let life = r.lifecycle.lock().unwrap();
        assert!(!life.permits("ring-2", Some(1)));
        assert!(!life.visible("ring-2", Some(1)));
    }
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
            samples: samples.to_vec().into(),
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
                assert_eq!(
                    p.samples.iter().copied().collect::<Vec<_>>(),
                    [1, 2, 3, 4, 5]
                );
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
