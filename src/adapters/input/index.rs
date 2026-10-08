use super::{AudioChunk, InputAdapter, InputEvent, interaction::Interaction, pcm::PcmInput};
use crate::{config, output::Output, recordings::Recordings, settings::Settings};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use pebble_index::reception::{
    button_detector::{ButtonHistory, Press, SourceClassification},
    session_state::Observation,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    time::Instant,
};

pub struct IndexInput {
    recordings: Recordings,
    interaction: Interaction,
    address: String,
    save_cursor: bool,
    history: ButtonHistory,
    classification: HashMap<String, SourceClassification>,
    initialized: bool,
    collecting: Option<(bool, u64)>,
    unread: u64,
    known_end: u64,
    parsed: BTreeSet<u64>,
    time: u64,
    sequence: Option<u64>,
    legacy_clock: Instant,
    completed: Vec<String>,
    committed: Option<u64>,
}
impl IndexInput {
    pub fn new(address: &str, save_cursor: bool, settings: Settings) -> Result<Self> {
        Ok(Self {
            recordings: Recordings::new(address),
            interaction: Interaction::new(&settings)?,
            address: address.into(),
            save_cursor,
            history: ButtonHistory::default(),
            classification: HashMap::new(),
            initialized: !save_cursor,
            collecting: None,
            unread: 0,
            known_end: 0,
            parsed: BTreeSet::new(),
            time: 0,
            sequence: None,
            legacy_clock: Instant::now(),
            completed: vec![],
            committed: None,
        })
    }
    fn observe(&mut self, event: Observation, output: &Output) -> Result<Vec<InputEvent>> {
        self.interaction
            .observe(self.time, event, &mut self.recordings, output)
    }
    fn acknowledgements(&mut self, output: &Output) -> Result<Vec<InputEvent>> {
        let mut events = vec![];
        for key in std::mem::take(&mut self.completed) {
            if let Some(id) = self.interaction.id(&key) {
                events.extend(self.observe(Observation::Recognized(id), output)?);
            } else if !self.save_cursor {
                self.recordings.release(&key);
            }
        }
        // Retired sources no longer need metadata-classification state.
        self.classification
            .retain(|key, _| self.interaction.source_session(key).is_some());
        Ok(events)
    }
    fn timestamp(&mut self, message: &Value) -> Result<()> {
        if let Some(sequence) = message["received_seq"].as_u64() {
            ensure!(
                self.sequence.is_none_or(|last| sequence > last),
                "Reception sequence regressed or was replayed"
            );
            self.sequence = Some(sequence);
        }
        let time = message["received_ms"]
            .as_u64()
            .unwrap_or_else(|| self.legacy_clock.elapsed().as_millis() as u64);
        ensure!(time >= self.time, "Reception timestamp regressed");
        self.time = time;
        Ok(())
    }
    fn advance_parsed(&mut self, floor: u64) {
        self.unread = self.unread.max(floor);
        self.parsed.retain(|index| *index >= self.unread);
        while self.parsed.remove(&self.unread) {
            self.unread += 1;
        }
    }
}
impl InputAdapter for IndexInput {
    fn decode(&mut self, message: Value, output: &Output) -> Result<Vec<InputEvent>> {
        self.timestamp(&message)?;
        let mut events = self.acknowledgements(output)?;
        match message["type"].as_str().context("Missing input type")? {
            "boundary" => {
                let index = u16::try_from(message["index"].as_u64().context("Missing boundary")?)?;
                self.recordings.reset(index)?;
                self.unread = self.recordings.position(index);
                self.known_end = self.unread;
                self.committed = Some(self.unread);
                self.initialized = true;
                if self.save_cursor
                    && let Some((true, first_seen)) = self.collecting
                {
                    events.extend(self.interaction.observe(
                        first_seen,
                        Observation::Collecting {
                            active: true,
                            unread: self.unread,
                        },
                        &mut self.recordings,
                        output,
                    )?);
                    events.extend(self.interaction.tick(
                        self.time,
                        &mut self.recordings,
                        output,
                    )?);
                }
            }
            "button_state" => {
                let active = message["pressed"]
                    .as_bool()
                    .context("Missing collecting state")?;
                if self.collecting.is_none_or(|(old, _)| old != active) {
                    self.collecting = Some((active, self.time));
                }
                if self.initialized && self.save_cursor {
                    if message["range_pending"] == true {
                        events.extend(self.observe(Observation::RangePending(true), output)?);
                    }
                    let unread = message["unread"]
                        .as_u64()
                        .map(|n| self.recordings.position(n as u16))
                        .unwrap_or(self.unread);
                    events
                        .extend(self.observe(Observation::Collecting { active, unread }, output)?);
                }
            }
            "range_pending" => {
                if self.initialized && self.save_cursor {
                    events.extend(self.observe(Observation::RangePending(true), output)?);
                }
            }
            "clock" => {
                if self.initialized && self.save_cursor {
                    events.extend(self.interaction.tick(
                        self.time,
                        &mut self.recordings,
                        output,
                    )?);
                }
            }
            "state" if self.save_cursor => {} // S is interpreted once, through button_state.
            "connection_lost" | "connected" => {
                if self.save_cursor && self.initialized {
                    events.extend(self.observe(
                        Observation::Connected(message["type"] == "connected"),
                        output,
                    )?);
                }
            }
            "range" => {
                let start =
                    u16::try_from(message["start"].as_u64().context("Missing range start")?)?;
                let end = u16::try_from(message["end"].as_u64().context("Missing range end")?)?;
                let lost = self.recordings.retain(start, end, output);
                self.known_end = self.known_end.max(self.recordings.position(end));
                for key in lost {
                    events.extend(if self.save_cursor {
                        self.observe(Observation::Lost(key), output)?
                    } else {
                        vec![InputEvent::Discard(key)]
                    });
                }
                // Missing collections before R.start can no longer arrive.
                // Loss is reported above; it must not hold every later tap's
                // metadata watermark forever. This does not complete any PCM.
                self.advance_parsed(self.recordings.position(start));
                if self.save_cursor {
                    events.extend(self.observe(
                        Observation::Watermark {
                            known_end: self.known_end,
                            processed_end: self.unread.min(self.known_end),
                        },
                        output,
                    )?);
                    events.extend(self.observe(Observation::RangePending(false), output)?);
                }
            }
            "collection" => {
                ensure!(
                    self.initialized,
                    "Collection arrived before startup boundary"
                );
                let started = Instant::now();
                let index = u16::try_from(message["index"].as_u64().context("Missing index")?)?;
                let raw =
                    STANDARD.decode(message["raw"].as_str().context("Missing raw collection")?)?;
                let parts = self.recordings.add(index, &raw, output)?;
                // The raw C was validated even if it was a retired/pre-start
                // source or awaits a gap in its source. Earlier missing C still
                // hold unread back until received or explicitly evicted by R.
                let position = self.recordings.position(index);
                if position >= self.unread {
                    self.parsed.insert(position);
                }
                let mut updates = std::collections::BTreeMap::new();
                for part in parts {
                    if !self.save_cursor {
                        events.push(InputEvent::Audio(AudioChunk {
                            key: part.key,
                            samples: part.samples,
                            rate: part.rate,
                            final_part: part.final_part,
                            checkpoint: Some(json!(part.next)),
                        }));
                        continue;
                    }
                    let position = self.recordings.position(part.index);
                    let buttons: Option<Vec<Press>> = part.buttons.as_ref().map(|b| {
                        b.iter()
                            .map(|b| {
                                if b == "long" {
                                    Press::Long
                                } else {
                                    Press::Short
                                }
                            })
                            .collect()
                    });
                    let delta = self.history.observe(position, buttons.as_deref())?;
                    let first = !self.classification.contains_key(&part.key);
                    let classification = self
                        .classification
                        .entry(part.key.clone())
                        .or_default()
                        .observe(&delta, first, part.final_part);
                    output.debug(format!("Button history source={} collection={} evidence={:?} added={:?} classification={classification:?} lifetime_count={:?}",part.key, part.index, delta.evidence, delta.added, part.lifetime_count));
                    // Gap completion can release several C records at once. Normalize
                    // all their histories before a completed short retires the source.
                    updates.insert(part.key.clone(), (classification, part.samples));
                    if position >= self.unread {
                        self.parsed.insert(position);
                    }
                }
                for (key, (classification, samples)) in updates {
                    let observation = self.recordings.observation(&key, classification)?;
                    events.extend(self.observe(Observation::Source(observation), output)?);
                    if !samples.is_empty()
                        && let Some(session) = self.interaction.source_session(&key)
                    {
                        events.push(InputEvent::Level {
                            key: self.interaction.key(session),
                            level: crate::audio_level::normalized_iter(samples.iter().copied()),
                        });
                    }
                }
                self.advance_parsed(self.unread);
                self.known_end = self.known_end.max(self.recordings.position(index) + 1);
                if self.save_cursor {
                    events.extend(self.observe(
                        Observation::Watermark {
                            known_end: self.known_end,
                            processed_end: self.unread.min(self.known_end),
                        },
                        output,
                    )?);
                }
                output.debug(format!(
                    "decoder collection={index} decode={:.3}s",
                    started.elapsed().as_secs_f64()
                ));
            }
            "caught_up" => {}
            "flush" if self.save_cursor => {
                self.interaction.ensure_flushed()?;
                events.push(InputEvent::Flush);
            }
            _ => events.extend(PcmInput.decode(message, output)?),
        }
        Ok(events)
    }
    fn poll(&mut self, output: &Output) -> Result<Vec<InputEvent>> {
        // Reception deadlines only advance on producer clock messages. IPC or
        // ASR delay cannot allow a consumer wall-clock timer to overtake input.
        self.acknowledgements(output)
    }
    fn completed(&mut self, key: &str) {
        self.completed.push(key.into());
    }
    fn end_input(&self) -> Result<()> {
        if self.save_cursor {
            self.interaction.ensure_flushed()?;
        }
        Ok(())
    }
    fn commit(&mut self, checkpoint: &Value) -> Result<()> {
        if self.save_cursor {
            let index = u16::try_from(checkpoint.as_u64().context("Invalid Index checkpoint")?)?;
            let position = self.recordings.position(index);
            if self.committed.is_none_or(|old| position > old) {
                config::save_cursor(&self.address, index)?;
                self.committed = Some(position);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> IndexInput {
        IndexInput::new("test-ring", true, Settings::default()).unwrap()
    }
    fn send(input: &mut IndexInput, mut value: Value) -> Vec<InputEvent> {
        value["received_ms"] = json!(input.time + 1);
        input
            .decode(value, &Output::new(false, None).unwrap())
            .unwrap()
    }
    fn chunk(index: u16, first: u32, final_part: bool) -> Value {
        let mut raw = 13u32.to_le_bytes().to_vec();
        raw.extend([82, 6, 0]);
        raw.extend(first.to_le_bytes());
        raw.extend([1, final_part as u8]);
        json!({"type":"collection","index":index,"raw":STANDARD.encode(raw)})
    }
    #[test]
    fn eviction_advances_metadata_watermark_but_keeps_the_source_failed() {
        let mut input = input();
        send(&mut input, json!({"type":"boundary","index":1}));
        send(&mut input, json!({"type":"range","start":1,"end":4}));
        send(&mut input, chunk(1, 1, false));
        send(&mut input, chunk(3, 1, true));
        assert_eq!(input.unread, 65538); // C2 is missing; C3 cannot overtake it.
        let events = send(&mut input, json!({"type":"range","start":3,"end":4}));
        assert_eq!(input.unread, 65540);
        assert!(input.parsed.is_empty());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, InputEvent::Reception {
            effect: pebble_index::reception::input_effects::Effect::View(view), ..
        } if view.failed))
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            InputEvent::Reception {
                effect: pebble_index::reception::input_effects::Effect::Batch(_),
                ..
            }
        )));
    }
    #[test]
    fn ignored_pre_start_audio_does_not_leave_an_unparsed_hole() {
        let mut input = input();
        send(&mut input, json!({"type":"boundary","index":1}));
        send(&mut input, json!({"type":"range","start":1,"end":2}));
        send(&mut input, chunk(1, 0, true));
        assert_eq!(input.unread, input.known_end);
        assert!(input.parsed.is_empty());
    }
    #[test]
    fn ordinary_out_of_order_pcm_still_waits_for_the_missing_collection() {
        let mut input = input();
        send(&mut input, json!({"type":"boundary","index":1}));
        send(&mut input, json!({"type":"range","start":1,"end":4}));
        send(&mut input, chunk(3, 1, true));
        assert_eq!(input.unread, 65537);
        send(&mut input, chunk(1, 1, false));
        assert_eq!(input.unread, 65538);
        send(&mut input, chunk(2, 1, false));
        assert_eq!(input.unread, 65540);
        assert!(input.parsed.is_empty());
    }
}
