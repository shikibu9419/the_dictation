//! Replay belongs to the recognition controller, not to the disposable model
//! process. PCM blocks remain shared with the input store. Only the failed
//! model is replaced; input decoding, gestures, and the other model keep running.
use super::*;
use crate::adapters::speech::run_control::{ExecutionControl, RestartableControl};
use tokio::sync::watch;

const MAX_REPLAY_SAMPLES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct EngineFault;
impl std::fmt::Display for EngineFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Speech worker requires recovery")
    }
}
impl std::error::Error for EngineFault {}

struct Journal {
    part: Part,
    queued: Instant,
    generation: Option<u64>,
    final_received: bool,
}

pub(super) struct Recovery {
    config: speech::EngineConfig,
    language: String,
    requests: watch::Receiver<u64>,
    slot: Option<Arc<RestartableControl>>,
    journal: Option<Journal>,
    pending: Option<Job>,
    prepared: bool,
    replay: bool,
    refresh_priority: bool,
    failures: u32,
    healthy_since: Option<Instant>,
    visible_cursor: Option<u64>,
    catchup_cursor: Option<u64>,
}
impl Recovery {
    pub fn new(
        config: speech::EngineConfig,
        language: String,
        control: Option<Arc<dyn ExecutionControl>>,
    ) -> Self {
        let (restart, requests) = watch::channel(0);
        Self {
            config,
            language,
            requests,
            slot: control.map(|control| Arc::new(RestartableControl::new(control, restart))),
            journal: None,
            pending: None,
            prepared: false,
            replay: false,
            refresh_priority: false,
            failures: 0,
            healthy_since: Some(Instant::now()),
            visible_cursor: None,
            catchup_cursor: None,
        }
    }
    pub fn control(&self) -> Option<Arc<dyn ExecutionControl>> {
        self.slot
            .as_ref()
            .map(|slot| slot.clone() as Arc<dyn ExecutionControl>)
    }
    pub fn final_received(&mut self) {
        if let Some(journal) = &mut self.journal
            && journal.part.final_part
        {
            journal.final_received = true;
            self.failures = 0;
        }
    }
    pub fn partial_visible(
        &mut self,
        key: &str,
        generation: Option<u64>,
        consumed: Option<u64>,
    ) -> bool {
        if !self
            .journal
            .as_ref()
            .is_some_and(|j| j.part.key == key && j.generation == generation)
        {
            return true;
        }
        let Some(consumed) = consumed else {
            return true;
        }; // Legacy adapters have no consumption cursor.
        if self.catchup_cursor.is_some_and(|cursor| consumed < cursor) {
            return false;
        }
        self.catchup_cursor = None;
        self.visible_cursor = Some(self.visible_cursor.unwrap_or(0).max(consumed));
        true
    }
    fn prepare(&mut self, part: &Part, queued: Instant, generation: Option<u64>) -> Result<()> {
        if self.prepared {
            return Ok(());
        }
        if let Some(journal) = &mut self.journal
            && journal.part.key == part.key
            && journal.generation == generation
        {
            ensure!(
                journal.part.rate == part.rate,
                "Sample rate changed within replay journal"
            );
            ensure!(
                journal
                    .part
                    .samples
                    .len()
                    .saturating_add(part.samples.len())
                    <= MAX_REPLAY_SAMPLES,
                "Speech replay PCM capacity exceeded; refusing to truncate audio"
            );
            journal.part.samples.append(&part.samples);
            journal.part.final_part = part.final_part;
            journal.part.checkpoint = part.checkpoint.clone();
        } else {
            ensure!(
                part.samples.len() <= MAX_REPLAY_SAMPLES,
                "Speech replay PCM capacity exceeded; refusing to truncate audio"
            );
            self.journal = Some(Journal {
                part: part.clone(),
                queued,
                generation,
                final_received: false,
            });
            self.visible_cursor = None;
            self.catchup_cursor = None;
        }
        self.prepared = true;
        Ok(())
    }
    fn retry_delay(&mut self) -> Duration {
        if self
            .healthy_since
            .take()
            .is_some_and(|since| since.elapsed() >= Duration::from_secs(30))
        {
            self.failures = 0;
        }
        self.failures = self.failures.saturating_add(1);
        Duration::from_millis(if self.failures == 1 {
            0
        } else {
            (500u64 << self.failures.saturating_sub(2).min(6)).min(30_000)
        })
    }
}

pub(super) async fn start_engine(
    config: &speech::EngineConfig,
    language: &str,
    mode: &str,
    output: &Output,
) -> Result<Box<dyn SpeechEngine>> {
    let mut engine = tokio::time::timeout(
        Duration::from_secs(240),
        speech::create(config, language, mode, output.clone()),
    )
    .await
    .context("Speech process creation timed out")??;
    let ready = tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            let event = engine.event().await?;
            match event.kind() {
                "ready" => {
                    output.debug(format!(
                        "[{mode}] {} ready: {language}; pid={:?}",
                        engine.name(),
                        engine.process_id()
                    ));
                    return Ok(());
                }
                "error" => bail!("Speech initialization: {}", serde_json::to_value(event)?),
                "status" => output.debug(format!(
                    "[{mode}] initializing: {}",
                    serde_json::to_value(event)?
                )),
                _ => {} // Warm-up text never belongs to a user recording.
            }
        }
    })
    .await
    .context("Speech initialization timed out")
    .and_then(|result| result);
    if let Err(error) = ready {
        engine
            .close()
            .await
            .context("Failed to close an unready speech process")?;
        return Err(error);
    }
    Ok(engine)
}

impl Speech {
    pub(super) async fn work(
        mut self,
        mut jobs: ipc::Receiver<Job>,
        input: Arc<Mutex<Box<dyn InputAdapter>>>,
    ) -> Result<()> {
        let mut restart = self.recovery.requests.clone();
        loop {
            restart.borrow_and_update();
            let result = tokio::select! {
                biased;
                changed = restart.changed(), if self.recovery.slot.is_some() => {
                    changed.context("Speech restart monitor closed")?;
                    Err(anyhow::anyhow!("Execution permit failed; restarting its owner")).context(EngineFault)
                }
                result = self.run(&mut jobs, &input) => result,
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) if error.downcast_ref::<UserCancelled>().is_some() => {
                    match self.cancel_current(&input).await {
                        Ok(()) => {}
                        Err(error) if error.downcast_ref::<EngineFault>().is_some() => {
                            self.recover(&error).await;
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) if error.downcast_ref::<EngineFault>().is_some() => {
                    // Final may already have been emitted before a permit-release
                    // failure. Commit exactly once instead of replaying that job.
                    if self
                        .recovery
                        .journal
                        .as_ref()
                        .is_some_and(|j| j.final_received)
                    {
                        self.finish_audio(&input)?;
                    }
                    self.recover(&error).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn recover(&mut self, error: &anyhow::Error) {
        let began = Instant::now();
        self.output.debug(format!(
            "[{}] ASR recovery begin recording={:?} error={error:#}",
            self.mode, self.key
        ));
        // Helper::close drops stdin and waits, then kills only this owned child
        // if necessary. Never grant a detached compute slot before it finishes.
        loop {
            match self.engine.close().await {
                Ok(()) => break,
                Err(error) => {
                    self.output
                        .debug(format!("[{}] ASR close failed: {error:#}", self.mode));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
        if let Some(slot) = &self.recovery.slot {
            slot.detached();
        }
        self.recovery.catchup_cursor = self.recovery.visible_cursor;
        self.key = None;
        self.generation = None;
        self.rate = None;
        self.samples = 0;
        self.segments.clear();
        self.first_input = None;
        self.first_result = false;
        loop {
            let delay = self.recovery.retry_delay();
            self.output.debug(format!(
                "[{}] ASR restart attempt={} delay_ms={}",
                self.mode,
                self.recovery.failures,
                delay.as_millis()
            ));
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let result = async {
                let mut engine = start_engine(
                    &self.recovery.config,
                    &self.recovery.language,
                    self.mode,
                    &self.output,
                )
                .await?;
                if let Some(slot) = &self.recovery.slot {
                    let control = engine
                        .control()
                        .context("Restarted model lost execution control")?;
                    if let Err(error) = slot.install(control).await {
                        let _ = engine.close().await;
                        return Err(error);
                    }
                }
                Ok::<_, anyhow::Error>(engine)
            }
            .await;
            match result {
                Ok(engine) => {
                    self.engine = engine;
                    self.recovery.replay = true;
                    self.recovery.refresh_priority = true;
                    self.recovery.healthy_since = Some(Instant::now());
                    self.output.debug(format!(
                        "[{}] ASR process recovered in {:.3}s; input reception retained",
                        self.mode,
                        began.elapsed().as_secs_f64()
                    ));
                    return;
                }
                Err(error) => self
                    .output
                    .debug(format!("[{}] ASR restart failed: {error:#}", self.mode)),
            }
        }
    }

    fn journal_permitted(&self) -> bool {
        self.recovery.journal.as_ref().is_some_and(|journal| {
            let life = self.lifecycle.lock().unwrap();
            !life.cancellation.is_cancelled(&journal.part.key)
                && (self.mode != "live" || life.permits(&journal.part.key, journal.generation))
        })
    }
    fn acknowledge_batch(
        &self,
        part: &Part,
        input: &Arc<Mutex<Box<dyn InputAdapter>>>,
    ) -> Result<()> {
        let mut adapter = input.lock().unwrap();
        if let Some(checkpoint) = &part.checkpoint {
            adapter.commit(checkpoint)?;
        }
        // Completion includes explicit cancellation, so receive-owned PCM can
        // be released once all collections are present. It never fabricates text.
        let mut life = self.lifecycle.lock().unwrap();
        life.cancellation.finish(&part.key);
        life.finished.insert(part.key.clone());
        life.retire(&part.key);
        adapter.completed(&part.key);
        Ok(())
    }
    async fn cancel_current(&mut self, input: &Arc<Mutex<Box<dyn InputAdapter>>>) -> Result<()> {
        self.output.debug(format!(
            "[{}] cancelling recognition recording={:?}",
            self.mode, self.key
        ));
        // A worker that cannot acknowledge cancel promptly is replaced; the
        // cancelled journal is never replayed after recovery.
        tokio::time::timeout(Duration::from_secs(2), self.cancel())
            .await
            .context("Speech cancellation acknowledgement timed out")
            .context(EngineFault)??;
        if self.mode == "batch"
            && let Some(journal) = &self.recovery.journal
        {
            self.acknowledge_batch(&journal.part, input)?;
        }
        self.recovery.journal = None;
        self.recovery.pending = None;
        self.recovery.prepared = false;
        self.recovery.replay = false;
        Ok(())
    }
    fn finish_audio(&mut self, input: &Arc<Mutex<Box<dyn InputAdapter>>>) -> Result<()> {
        if self.mode == "batch" {
            let journal = self
                .recovery
                .journal
                .as_ref()
                .context("Missing completed speech journal")?;
            ensure!(
                journal.final_received,
                "Batch ended without a final result; PCM remains retained"
            );
            self.acknowledge_batch(&journal.part, input)?;
            self.recovery.journal = None;
        } else if !self.journal_permitted() {
            self.recovery.journal = None;
        }
        self.recovery.pending = None;
        self.recovery.prepared = false;
        Ok(())
    }

    async fn run(
        &mut self,
        jobs: &mut ipc::Receiver<Job>,
        input: &Arc<Mutex<Box<dyn InputAdapter>>>,
    ) -> Result<()> {
        if self.recovery.refresh_priority {
            let active = self.mode == "live" && self.journal_permitted();
            if let Some(priority) = &mut self.priority {
                priority.reconcile(active).await.context(EngineFault)?;
            }
            self.recovery.refresh_priority = false;
        }
        if self.recovery.replay {
            if self.journal_permitted() {
                let journal = self.recovery.journal.as_ref().unwrap();
                let (part, queued, generation) =
                    (journal.part.clone(), journal.queued, journal.generation);
                self.output.debug(format!(
                    "[{}] ASR replay recording={} generation={generation:?} samples=0..{}",
                    self.mode,
                    part.key,
                    part.samples.len()
                ));
                self.feed(part, queued, generation).await?;
                if self.recovery.prepared {
                    self.finish_audio(input)?;
                }
            } else {
                if self.mode == "batch"
                    && let Some(journal) = &self.recovery.journal
                {
                    self.acknowledge_batch(&journal.part, input)?;
                }
                self.recovery.journal = None;
                // A release/reset can invalidate a job while the model loads.
                if self.recovery.prepared {
                    self.recovery.pending = None;
                    self.recovery.prepared = false;
                }
            }
            self.recovery.replay = false;
        }
        loop {
            if self.recovery.pending.is_none() {
                tokio::select! {
                    biased;
                    job = jobs.recv() => {
                        let Some(job) = job? else { self.engine.close().await?; return Ok(()) };
                        self.recovery.pending = Some(job);
                    }
                    event = self.engine.event() => { self.event(event.context(EngineFault)?)?; continue; }
                }
            }
            match self.recovery.pending.as_ref().unwrap() {
                Job::Audio(..) | Job::ReceptionAudio(..) => {
                    let (part, queued, generation) = match self.recovery.pending.as_ref().unwrap() {
                        Job::Audio(part, queued) => (part.clone(), *queued, None),
                        Job::ReceptionAudio(part, queued, generation) => {
                            (part.clone(), *queued, Some(*generation))
                        }
                        _ => unreachable!(),
                    };
                    if self
                        .lifecycle
                        .lock()
                        .unwrap()
                        .cancellation
                        .is_cancelled(&part.key)
                    {
                        if self.mode == "batch" {
                            self.acknowledge_batch(&part, input)?;
                        }
                        self.recovery.pending = None;
                        self.recovery.prepared = false;
                        continue;
                    }
                    if self.mode == "live"
                        && !self
                            .lifecycle
                            .lock()
                            .unwrap()
                            .permits(&part.key, generation)
                    {
                        self.recovery.pending = None;
                        self.recovery.prepared = false;
                        continue;
                    }
                    self.recovery.prepare(&part, queued, generation)?;
                    self.output.debug(format!(
                        "latency {} input wait recording={}: {:.3}s",
                        self.mode,
                        part.key,
                        queued.elapsed().as_secs_f64()
                    ));
                    self.feed(part, queued, generation).await?;
                    self.finish_audio(input)?;
                }
                Job::Release(key) => {
                    let key = key.clone();
                    if self.key.as_ref() == Some(&key)
                        && (self.generation.is_none()
                            || !self
                                .lifecycle
                                .lock()
                                .unwrap()
                                .permits(&key, self.generation))
                    {
                        self.cancel().await?;
                        self.recovery.journal = None;
                    }
                    let mut life = self.lifecycle.lock().unwrap();
                    if life.reception.contains_key(&key)
                        || life.released.contains(&key)
                        || life.finished.contains(&key)
                    {
                        life.closed.insert(key.clone());
                        life.retire(&key);
                    }
                    self.recovery.pending = None;
                }
                Job::Reset(key, generation) => {
                    if self.key.as_ref() == Some(key) && self.generation != Some(*generation) {
                        self.cancel().await?;
                        self.recovery.journal = None;
                    }
                    self.recovery.pending = None;
                }
                Job::Checkpoint(value) => {
                    input.lock().unwrap().commit(value)?;
                    self.recovery.pending = None;
                }
                Job::Flush(_) => {
                    if let Some(Job::Flush(done)) = self.recovery.pending.take() {
                        let _ = done.send(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn part(key: &str, pcm: &[i16], final_part: bool) -> Part {
        Part {
            key: key.into(),
            samples: pcm.to_vec().into(),
            rate: 16_000,
            final_part,
            checkpoint: None,
        }
    }
    #[test]
    fn preparing_a_pending_job_twice_does_not_duplicate_pcm() {
        let mut r = Recovery::new(speech::EngineConfig::Apple, "ja_JP".into(), None);
        let now = Instant::now();
        let first = part("a", &[1, 2], false);
        r.prepare(&first, now, Some(0)).unwrap();
        r.prepare(&first, now, Some(0)).unwrap();
        assert_eq!(r.journal.as_ref().unwrap().part.samples.len(), 2);
        r.prepared = false;
        r.prepare(&part("a", &[3], true), now, Some(0)).unwrap();
        assert_eq!(
            r.journal
                .as_ref()
                .unwrap()
                .part
                .samples
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        r.final_received();
        assert!(r.journal.as_ref().unwrap().final_received);
    }
    #[test]
    fn a_new_generation_replaces_the_old_replay_and_retry_delay_is_bounded() {
        let mut r = Recovery::new(speech::EngineConfig::Apple, "ja_JP".into(), None);
        let now = Instant::now();
        r.prepare(&part("a", &[1, 2], false), now, Some(0)).unwrap();
        r.prepared = false;
        r.prepare(&part("a", &[3], false), now, Some(1)).unwrap();
        assert_eq!(
            r.journal
                .as_ref()
                .unwrap()
                .part
                .samples
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [3]
        );
        r.final_received(); // An unsolicited live final cannot acknowledge full audio.
        assert!(!r.journal.as_ref().unwrap().final_received);
        assert_eq!(r.retry_delay(), Duration::ZERO);
        assert_eq!(r.retry_delay(), Duration::from_millis(500));
        for _ in 0..100 {
            assert!(r.retry_delay() <= Duration::from_secs(30));
        }
    }

    #[test]
    fn native_replay_cannot_replace_visible_text_with_an_earlier_audio_cursor() {
        let mut r = Recovery::new(speech::EngineConfig::Apple, "ja_JP".into(), None);
        r.prepare(&part("a", &[1, 2, 3], false), Instant::now(), Some(0))
            .unwrap();
        assert!(r.partial_visible("a", Some(0), Some(3)));
        r.catchup_cursor = r.visible_cursor;
        assert!(!r.partial_visible("a", Some(0), Some(1)));
        assert!(r.partial_visible("a", Some(0), Some(3)));
        assert!(r.catchup_cursor.is_none());
        r.prepared = false;
        r.prepare(&part("b", &[4], false), Instant::now(), Some(0))
            .unwrap();
        assert!(r.partial_visible("b", Some(0), Some(1)));
    }

    struct Completed(Arc<Mutex<Vec<String>>>);
    impl InputAdapter for Completed {
        fn decode(&mut self, _: Value, _: &Output) -> Result<Vec<InputEvent>> {
            Ok(vec![])
        }
        fn completed(&mut self, key: &str) {
            self.0.lock().unwrap().push(key.into());
        }
    }
    async fn wait_log(path: &std::path::Path, predicate: impl Fn(&str) -> bool) {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if predicate(&std::fs::read_to_string(path).unwrap_or_default()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("native recovery log deadline");
    }
    #[tokio::test]
    #[ignore = "Pinned native Qwen model and synthetic WAV only; kills only the two model children this test starts; no UI/BLE/microphone"]
    async fn native_live_and_paused_batch_recover_independently_with_shared_permit_bindings() {
        assert!(
            std::env::var_os("INDEX_VOICE_SPEECH_COMMAND").is_none(),
            "Run without an external ASR override"
        );
        let model = std::env::var_os("INDEX_QWEN_MODEL").expect("Set INDEX_QWEN_MODEL");
        let fixtures = std::env::var_os("INDEX_QWEN_FIXTURES").expect("Set INDEX_QWEN_FIXTURES");
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(model, root.path().join("model")).unwrap();
        let log = root.path().join("recovery.log");
        let output = Output::new(false, Some(&log)).unwrap();
        crate::qwen_setup::setup_model(root.path().into(), output.clone())
            .await
            .unwrap();
        let config = speech::EngineConfig::Qwen {
            root: root.path().into(),
        };
        let options = Options {
            address: "pcm".into(),
            language: "ja_JP".into(),
            command: "stream".into(),
            verbose: true,
        };
        let life = Arc::new(Mutex::new(Lifecycle {
            collecting: Some(true),
            ..Default::default()
        }));
        let mut live = Speech::start("live", &config, &options, life.clone(), output.clone())
            .await
            .unwrap();
        let batch = Speech::start("batch", &config, &options, life.clone(), output.clone())
            .await
            .unwrap();
        let live_pid = live.engine.process_id().unwrap();
        let batch_pid = batch.engine.process_id().unwrap();
        live.priority = Some(
            LivePriority::new(live.recovery.control(), batch.recovery.control().unwrap())
                .await
                .unwrap(),
        );
        let completed = Arc::new(Mutex::new(vec![]));
        let input: Arc<Mutex<Box<dyn InputAdapter>>> =
            Arc::new(Mutex::new(Box::new(Completed(completed.clone()))));
        let mut reader =
            hound::WavReader::open(std::path::Path::new(&fixtures).join("short.wav")).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        let pcm: pebble_core::pcm::Pcm = reader.samples::<i16>().map(|n| n.unwrap()).collect();
        let audio = |key: &str, final_part| Part {
            key: key.into(),
            samples: pcm.clone(),
            rate: 16_000,
            final_part,
            checkpoint: None,
        };
        let (ltx, lrx) = ipc::process_channel("speech jobs");
        let (btx, brx) = ipc::process_channel("speech jobs");
        let mut tasks = JoinSet::new();
        tasks.spawn(live.work(lrx, input.clone()));
        tasks.spawn(batch.work(brx, input));
        ltx.send(Job::Audio(audio("first", false), Instant::now()))
            .unwrap();
        wait_log(&log, |s| {
            s.contains("[live] partial recording=Some(\"first\")")
        })
        .await;
        btx.send(Job::Audio(audio("first", true), Instant::now()))
            .unwrap();
        wait_log(&log, |s| s.contains("[batch] engine input: finish")).await;
        // Only the PID returned by this test's own SpeechEngine is signalled.
        assert_eq!(unsafe { libc::kill(batch_pid as i32, libc::SIGKILL) }, 0);
        wait_log(&log, |s| s.contains("[batch] ASR process recovered")).await;
        assert!(
            completed.lock().unwrap().is_empty(),
            "Replacement batch must remain paused behind live"
        );
        life.lock().unwrap().released.insert("first".into());
        ltx.send(Job::Release("first".into())).unwrap();
        wait_log(&log, |_| completed.lock().unwrap().len() == 1).await;

        ltx.send(Job::Audio(audio("second", false), Instant::now()))
            .unwrap();
        wait_log(&log, |s| {
            s.contains("[live] partial recording=Some(\"second\")")
        })
        .await;
        assert_eq!(unsafe { libc::kill(live_pid as i32, libc::SIGKILL) }, 0);
        wait_log(&log, |s| s.contains("[live] ASR process recovered")).await;
        wait_log(&log, |s| {
            s.matches("[live] partial recording=Some(\"second\")")
                .count()
                >= 2
        })
        .await;
        btx.send(Job::Audio(audio("second", true), Instant::now()))
            .unwrap();
        life.lock().unwrap().released.insert("second".into());
        ltx.send(Job::Release("second".into())).unwrap();
        wait_log(&log, |_| completed.lock().unwrap().len() == 2).await;
        assert_eq!(*completed.lock().unwrap(), ["first", "second"]);
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.matches("[live] ASR process recovered").count(), 1);
        assert_eq!(text.matches("[batch] ASR process recovered").count(), 1);
        for key in ["first", "second"] {
            let final_line = text
                .lines()
                .find(|line| line.contains(&format!("[batch] final recording=Some(\"{key}\")")))
                .unwrap();
            assert!(
                final_line.contains("これは音声認識の動作確認です"),
                "{final_line}"
            );
        }
        drop(ltx);
        drop(btx);
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(result) = tasks.join_next().await {
                result.unwrap().unwrap();
            }
        })
        .await
        .unwrap();
    }
}
