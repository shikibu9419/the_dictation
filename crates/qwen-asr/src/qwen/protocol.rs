//! Versioned JSONL control plane. It never executes model or GPU operations.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{BufRead, Read, Write},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

pub const VERSION: u32 = 2;
const MAX_LINE: usize = 16 * 1024 * 1024;
const MAX_PENDING_SECONDS: usize = 300;
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tag {
    pub session_id: u64,
    pub generation: u64,
}
#[derive(Deserialize)]
struct Wire {
    protocol_version: Option<u32>,
    session_id: Option<u64>,
    generation: Option<u64>,
    #[serde(flatten)]
    command: Command,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Audio { pcm: String, sample_rate: u32 },
    Finish,
    Cancel,
    Permit { enabled: bool, request: Option<u64> },
}

pub struct Sink(Mutex<Box<dyn Write + Send>>);
impl Sink {
    pub fn new(output: impl Write + Send + 'static) -> Self {
        Self(Mutex::new(Box::new(output)))
    }
    pub fn send(&self, value: &Value) -> Result<()> {
        let mut output = self.0.lock().unwrap();
        serde_json::to_writer(&mut *output, value)?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    }
}
struct State {
    epoch: u64,
    tag: Tag,
    active: bool,
    reset_pending: bool,
    rate: Option<u32>,
    pending: VecDeque<i16>,
    accepted: u64,
    delivered: u64,
    finish: bool,
    permitted: bool,
    permit_request: u64,
    busy: bool,
    paused: bool,
    paused_time: Duration,
    closed: bool,
    failure: Option<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            epoch: 0,
            tag: Tag::default(),
            active: false,
            reset_pending: false,
            rate: None,
            pending: VecDeque::new(),
            accepted: 0,
            delivered: 0,
            finish: false,
            permitted: true,
            permit_request: 0,
            busy: true, // Loading/warmup belongs to the inference owner too.
            paused: false,
            paused_time: Duration::ZERO,
            closed: false,
            failure: None,
        }
    }
}
impl State {
    fn reset(&mut self) {
        self.epoch += 1;
        self.active = false;
        self.reset_pending = true;
        self.rate = None;
        self.pending.clear();
        self.accepted = 0;
        self.delivered = 0;
        self.finish = false;
    }
    fn envelope(&self, mut value: Value) -> Value {
        value["protocol_version"] = json!(VERSION);
        value["session_id"] = json!(self.tag.session_id);
        value["generation"] = json!(self.tag.generation);
        value["accepted_samples"] = json!(self.accepted);
        value["sample_rate"] = json!(self.rate.unwrap_or(16000));
        value
    }
    fn permit_ack(&self) -> Value {
        // Worker-wide control is independent of recording identity. A cancel
        // must not make a pause acknowledgement appear to belong to stale PCM.
        json!({"type":"status","protocol_version":VERSION,
            "text":"Qwen execution permission", "permit_request":self.permit_request,
            "permitted":self.permitted, "paused":!self.permitted && (!self.busy || self.paused)})
    }
}
pub struct Input {
    state: Mutex<State>,
    changed: Condvar,
    pub sink: Sink,
}
pub struct Work {
    pub reset_only: bool,
    pub epoch: u64,
    pub rate: u32,
    pub audio: Vec<f32>,
    pub final_input: bool,
    pub delivered: u64,
}
impl Input {
    pub fn new(sink: Sink) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            sink,
        })
    }
    fn command(&self, wire: Wire) -> Result<()> {
        ensure!(
            wire.protocol_version.is_none_or(|v| v == VERSION),
            "Unsupported protocol version"
        );
        let tag = match (wire.session_id, wire.generation) {
            (Some(session_id), Some(generation)) => Some(Tag {
                session_id,
                generation,
            }),
            (None, None) => None,
            _ => anyhow::bail!("session_id and generation must be supplied together"),
        };
        let mut s = self.state.lock().unwrap();
        if s.closed {
            return Ok(());
        }
        if !matches!(wire.command, Command::Permit { .. })
            && let Some(tag) = tag
        {
            ensure!(
                !s.active || s.tag == tag,
                "Session changed without finish/cancel"
            );
            if !s.active {
                s.tag = tag;
            }
        }
        match wire.command {
            Command::Audio { pcm, sample_rate } => {
                ensure!((1000..=192000).contains(&sample_rate), "Invalid PCM rate");
                ensure!(!s.finish, "Audio after finish");
                ensure!(
                    s.rate.is_none_or(|r| r == sample_rate),
                    "Sample rate changed within recording"
                );
                let bytes = STANDARD.decode(pcm).context("Invalid base64 PCM")?;
                ensure!(bytes.len() % 2 == 0, "Odd PCM byte count");
                ensure!(
                    s.pending.len() + bytes.len() / 2 <= sample_rate as usize * MAX_PENDING_SECONDS,
                    "Recognition queue exceeds 300 seconds; audio was not silently dropped"
                );
                s.pending.extend(
                    bytes
                        .chunks_exact(2)
                        .map(|v| i16::from_le_bytes([v[0], v[1]])),
                );
                s.active = true;
                s.rate = Some(sample_rate);
                s.accepted += (bytes.len() / 2) as u64;
                self.sink.send(&s.envelope(json!({"type":"accepted"})))?;
            }
            Command::Finish => {
                ensure!(!s.finish, "Duplicate finish");
                s.active = true;
                s.finish = true;
            }
            Command::Cancel => {
                let cancelled = s.envelope(json!({"type":"cancelled"}));
                s.reset();
                self.sink.send(&cancelled)?;
                s.tag.generation += 1;
            }
            Command::Permit { enabled, request } => {
                let request = request.unwrap_or(s.permit_request + 1);
                ensure!(request > s.permit_request, "Out-of-order permit request");
                s.permit_request = request;
                s.permitted = enabled;
                self.sink.send(&s.permit_ack())?;
            }
        }
        self.changed.notify_all();
        Ok(())
    }
    pub fn read(self: Arc<Self>, mut reader: impl BufRead) {
        let result = (|| -> Result<()> {
            loop {
                let mut line = Vec::new();
                let count = (&mut reader)
                    .take((MAX_LINE + 1) as u64)
                    .read_until(b'\n', &mut line)?;
                if count == 0 {
                    return Ok(());
                }
                ensure!(
                    line.len() <= MAX_LINE,
                    "Speech protocol line exceeds 16 MiB"
                );
                let command: Wire =
                    serde_json::from_slice(&line).context("Invalid speech command")?;
                self.command(command)?;
            }
        })();
        let mut s = self.state.lock().unwrap();
        s.closed = true;
        s.epoch += 1;
        if let Err(e) = result {
            s.failure = Some(format!("{e:#}"));
        }
        self.changed.notify_all();
    }
    pub fn shutdown_result(&self) -> Result<()> {
        let s = self.state.lock().unwrap();
        if let Some(error) = &s.failure {
            anyhow::bail!("{error}");
        }
        Ok(())
    }
    pub fn closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }
    pub fn current(&self, epoch: u64) -> bool {
        let s = self.state.lock().unwrap();
        !s.closed && s.epoch == epoch
    }
    /// Cooperative scheduling: a paused batch retains its in-progress model state.
    pub fn checkpoint(&self, epoch: u64, synchronize: impl FnOnce()) -> bool {
        let mut s = self.state.lock().unwrap();
        let mut pause_began = None;
        if !s.closed && s.epoch == epoch && !s.permitted {
            // This callback runs on the model thread. Synchronize outstanding
            // Metal work before claiming that another worker may use the GPU.
            drop(s);
            synchronize();
            s = self.state.lock().unwrap();
        }
        while !s.closed && s.epoch == epoch && !s.permitted {
            if !s.paused {
                s.paused = true;
                pause_began = Some(Instant::now());
                if self.sink.send(&s.permit_ack()).is_err() {
                    s.closed = true;
                    break;
                }
            }
            s = self.changed.wait(s).unwrap();
        }
        if let Some(began) = pause_began {
            s.paused_time += began.elapsed();
        }
        s.paused = false;
        s.closed || s.epoch != epoch
    }
    pub fn paused_seconds(&self) -> f64 {
        self.state.lock().unwrap().paused_time.as_secs_f64()
    }
    pub fn idle(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        let was_busy = s.busy;
        s.busy = false;
        s.paused = false;
        if was_busy && !s.permitted {
            self.sink.send(&s.permit_ack())?;
        }
        Ok(())
    }
    pub fn take(&self) -> Result<Option<Work>> {
        self.idle()?;
        let mut s = self.state.lock().unwrap();
        while !s.closed && s.pending.is_empty() && !s.finish && !s.reset_pending {
            s = self.changed.wait(s).unwrap();
        }
        if let Some(error) = &s.failure {
            anyhow::bail!("{error}");
        }
        if s.closed {
            return Ok(None);
        }
        if s.reset_pending {
            s.reset_pending = false;
            return Ok(Some(Work {
                reset_only: true,
                epoch: s.epoch,
                rate: s.rate.unwrap_or(16000),
                audio: Vec::new(),
                final_input: false,
                delivered: 0,
            }));
        }
        let rate = s.rate.unwrap_or(16000);
        s.busy = true;
        // Coalesce pending packets, bounding each catch-up turn to two seconds.
        let count = s.pending.len().min(rate as usize * 2);
        let audio = s
            .pending
            .drain(..count)
            .map(|v| v as f32 / 32768.0)
            .collect();
        s.delivered += count as u64;
        Ok(Some(Work {
            reset_only: false,
            epoch: s.epoch,
            rate,
            audio,
            final_input: s.finish && s.pending.is_empty(),
            delivered: s.delivered,
        }))
    }
    pub fn publish(&self, epoch: u64, value: Value) -> Result<bool> {
        let s = self.state.lock().unwrap();
        if s.closed || s.epoch != epoch {
            return Ok(false);
        }
        self.sink.send(&s.envelope(value))?;
        Ok(true)
    }
    pub fn finish(&self, epoch: u64, text: &str, consumed: u64) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        if s.closed || s.epoch != epoch {
            return Ok(());
        }
        ensure!(
            s.finish && s.pending.is_empty() && consumed == s.accepted,
            "Final result does not cover all accepted PCM"
        );
        self.sink
            .send(&s.envelope(json!({"type":"final","text":text,"consumed_samples":consumed})))?;
        s.reset();
        s.tag.session_id += 1;
        s.tag.generation += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> Arc<Input> {
        Input::new(Sink::new(Vec::<u8>::new()))
    }
    fn command(input: &Input, value: Value) -> Result<()> {
        input.command(serde_json::from_value(value)?)
    }
    #[test]
    fn accepted_does_not_mean_consumed_and_cancel_invalidates_work() {
        let i = input();
        command(&i,json!({"type":"audio","pcm":"AAABAA==","sample_rate":16000,"session_id":7,"generation":1})).unwrap();
        assert_eq!(i.state.lock().unwrap().delivered, 0);
        let work = i.take().unwrap().unwrap();
        assert_eq!(work.audio.len(), 2);
        command(&i, json!({"type":"cancel","session_id":7,"generation":1})).unwrap();
        assert!(!i.current(work.epoch));
        assert!(
            !i.publish(work.epoch, json!({"type":"partial","text":"stale"}))
                .unwrap()
        );
        command(
            &i,
            json!({"type":"audio","pcm":"AAA=","sample_rate":9997,"session_id":8,"generation":2}),
        )
        .unwrap();
        assert_ne!(i.take().unwrap().unwrap().epoch, work.epoch);
    }
    #[test]
    fn bad_rate_odd_pcm_and_changed_generation_are_rejected() {
        let i = input();
        assert!(command(&i, json!({"type":"audio","pcm":"AAA=","sample_rate":0})).is_err());
        assert!(command(&i, json!({"type":"audio","pcm":"AA==","sample_rate":16000})).is_err());
        command(&i, json!({"type":"audio","pcm":"AAA=","sample_rate":16000})).unwrap();
        assert!(command(&i, json!({"type":"audio","pcm":"AAA=","sample_rate":9997})).is_err());
        assert!(command(&i, json!({"type":"finish","session_id":99,"generation":1})).is_err());
    }
    #[test]
    fn pause_is_released_by_cancel_and_eof() {
        let i = input();
        command(&i, json!({"type":"permit","enabled":false})).unwrap();
        let epoch = i.state.lock().unwrap().epoch;
        let child = i.clone();
        let task = std::thread::spawn(move || child.checkpoint(epoch, || {}));
        command(&i, json!({"type":"cancel"})).unwrap();
        assert!(task.join().unwrap());
        i.clone().read(std::io::Cursor::new(b""));
        assert!(i.take().unwrap().is_none());
    }
    #[test]
    fn final_requires_every_accepted_sample() {
        let i = input();
        command(
            &i,
            json!({"type":"audio","pcm":"AAABAA==","sample_rate":16000}),
        )
        .unwrap();
        command(&i, json!({"type":"finish"})).unwrap();
        let w = i.take().unwrap().unwrap();
        assert!(i.finish(w.epoch, "text", 1).is_err());
        i.finish(w.epoch, "text", 2).unwrap();
        assert!(!i.current(w.epoch));
    }
    #[test]
    fn pause_ack_waits_for_gpu_boundary_and_idle_ack_is_immediate() {
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };
        let i = input();
        command(&i, json!({"type":"permit","enabled":false,"request":7})).unwrap();
        assert_eq!(i.state.lock().unwrap().permit_ack()["paused"], false);
        let (entered, entry) = mpsc::channel();
        let (resume, gate) = mpsc::channel();
        let child = i.clone();
        let task = std::thread::spawn(move || {
            child.checkpoint(0, || {
                entered.send(()).unwrap();
                gate.recv().unwrap();
            })
        });
        entry.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(i.state.lock().unwrap().permit_ack()["paused"], false);
        resume.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !i.state.lock().unwrap().paused {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        command(&i, json!({"type":"permit","enabled":false,"request":8})).unwrap();
        let ack = i.state.lock().unwrap().permit_ack();
        assert_eq!(ack["permit_request"], 8);
        assert_eq!(ack["paused"], true);
        assert!(ack.get("session_id").is_none());
        command(&i, json!({"type":"cancel"})).unwrap();
        assert!(task.join().unwrap());
        i.idle().unwrap();
        command(&i, json!({"type":"permit","enabled":false,"request":9})).unwrap();
        assert_eq!(i.state.lock().unwrap().permit_ack()["paused"], true);
        assert!(command(&i, json!({"type":"permit","enabled":true,"request":8})).is_err());
    }
}
