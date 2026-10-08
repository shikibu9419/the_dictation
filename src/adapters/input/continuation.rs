//! Join Index audio segments across brief releases. Times are receiver-side;
//! never infer physical button timing from buffered audio after a disconnect.
use super::{AudioChunk, InputEvent};
use crate::output::Output;
use anyhow::{Result, ensure};
use serde_json::Value;
use std::time::{Duration, Instant};

struct Session {
    key: String,
    source: String,
    rate: u32,
    checkpoint: Option<Value>,
    ended: Option<Instant>,
    short_segments: usize,
    source_samples: usize,
}
pub struct Continuation {
    grace: Duration,
    current: Option<Session>,
    pressed: bool,
    separated: bool,
    released: Option<Instant>,
    recovering: bool,
}
impl Continuation {
    pub fn new(grace: Duration) -> Self {
        Self {
            grace,
            current: None,
            pressed: false,
            separated: false,
            released: None,
            recovering: false,
        }
    }
    fn finish(&mut self, output: &Output) -> Vec<InputEvent> {
        let Some(s) = self.current.take() else {
            return vec![];
        };
        output.debug(format!(
            "Continuation finish session={} short_segments={}",
            s.key, s.short_segments
        ));
        vec![
            InputEvent::State(false),
            InputEvent::Audio(AudioChunk {
                key: s.key,
                samples: vec![],
                rate: s.rate,
                final_part: true,
                checkpoint: s.checkpoint,
            }),
        ]
    }
    pub fn poll(&mut self, output: &Output) -> Vec<InputEvent> {
        if self
            .current
            .as_ref()
            .is_some_and(|s| s.ended.is_some_and(|t| t.elapsed() >= self.grace))
            && !self.pressed
        {
            self.finish(output)
        } else {
            vec![]
        }
    }
    pub fn connection_lost(&mut self, output: &Output) -> Vec<InputEvent> {
        self.recovering = true;
        self.separated = true;
        self.pressed = false;
        output.debug("Continuation disabled during BLE recovery; preserve current source, separate later recordings");
        if self.current.as_ref().is_some_and(|s| s.ended.is_some()) {
            self.finish(output)
        } else {
            vec![]
        }
    }
    pub fn caught_up(&mut self, output: &Output) {
        if self.recovering {
            output.debug("Continuation enabled after BLE backlog drained");
            self.recovering = false;
        }
    }
    pub fn apply(&mut self, event: InputEvent, output: &Output) -> Result<Vec<InputEvent>> {
        if self.grace.is_zero() {
            return Ok(vec![event]);
        }
        let mut result = self.poll(output);
        match event {
            event @ InputEvent::Activity { .. } => result.push(event),
            InputEvent::Checkpoint(value) => result.push(InputEvent::Checkpoint(value)),
            InputEvent::Gesture(event) => result.push(InputEvent::Gesture(event)),
            InputEvent::State(pressed) => {
                if pressed && !self.pressed {
                    if self.released.is_some_and(|t| t.elapsed() >= self.grace) {
                        self.separated = true;
                        if self.current.as_ref().is_some_and(|s| s.ended.is_some()) {
                            result.extend(self.finish(output));
                        }
                    }
                    if self.current.is_none() {
                        result.push(InputEvent::State(true));
                    }
                    output.debug(format!(
                        "Continuation press: resume={} gap_ms={:?}",
                        self.current.is_some() && !self.separated,
                        self.released.map(|t| t.elapsed().as_millis())
                    ));
                } else if !pressed && self.pressed {
                    self.released = Some(Instant::now());
                    output.debug(format!(
                        "Continuation release: grace_ms={}",
                        self.grace.as_millis()
                    ));
                }
                self.pressed = pressed;
            }
            InputEvent::Audio(mut part) => {
                let next_source = self.current.as_ref().is_some_and(|s| s.source != part.key);
                if next_source && (self.separated || self.recovering) {
                    ensure!(
                        self.current.as_ref().unwrap().ended.is_some(),
                        "New Index recording arrived before previous final segment"
                    );
                    result.extend(self.finish(output));
                }
                if self.current.is_none() {
                    result.push(InputEvent::State(true));
                    self.current = Some(Session {
                        key: part.key.clone(),
                        source: part.key.clone(),
                        rate: part.rate,
                        checkpoint: None,
                        ended: None,
                        short_segments: 0,
                        source_samples: 0,
                    });
                    self.separated = false;
                }
                let s = self.current.as_mut().unwrap();
                ensure!(
                    s.rate == part.rate,
                    "Sample rate changed during continued recording"
                );
                if s.source != part.key {
                    ensure!(
                        s.ended.is_some(),
                        "Interleaved Index recordings cannot be joined"
                    );
                    output.debug(format!(
                        "Continuation join session={} source={} -> {}; short_segments={}",
                        s.key, s.source, part.key, s.short_segments
                    ));
                    s.source = part.key.clone();
                    s.source_samples = 0;
                }
                s.ended = None;
                let before = s.source_samples;
                s.source_samples += part.samples.len();
                if s.short_segments > 0
                    && before * 1000 < s.rate as usize * 150
                    && s.source_samples * 1000 >= s.rate as usize * 150
                {
                    output.debug(format!(
                        "Continuation short-to-hold session={} preceding_short_segments={}",
                        s.key, s.short_segments
                    ));
                }
                if part.final_part {
                    if s.source_samples * 1000 < s.rate as usize * 150 {
                        s.short_segments += 1;
                    }
                    s.ended = Some(Instant::now());
                    s.checkpoint = part.checkpoint.take();
                    output.debug(format!("Continuation awaiting resume session={} source={} grace_ms={} short_segments={}", s.key, s.source, self.grace.as_millis(), s.short_segments));
                }
                let was_final = part.final_part;
                part.key = s.key.clone();
                part.final_part = false;
                result.push(InputEvent::Audio(part));
                if was_final && (self.recovering || self.separated) {
                    result.extend(self.finish(output));
                }
            }
            InputEvent::Discard(key) => {
                if self.current.as_ref().is_some_and(|s| s.source == key) {
                    result.push(InputEvent::Discard(self.current.take().unwrap().key));
                    result.push(InputEvent::State(false));
                } else {
                    result.push(InputEvent::Discard(key));
                }
            }
            InputEvent::Flush => {
                if self.current.as_ref().is_some_and(|s| s.ended.is_some()) {
                    result.extend(self.finish(output));
                }
                result.push(InputEvent::Flush);
            }
        }
        Ok(result)
    }
}
