use super::InputEvent;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    Recording,
    Dictating,
    Error,
}

const HOLD_THRESHOLD: Duration = Duration::from_millis(150);

/// Desktop interaction state. Unclassified button edges never activate the UI.
#[derive(Default)]
pub struct Interaction {
    recordings: HashSet<String>,
    state: State,
    pressed_since: Option<Instant>,
    pending: HashSet<String>,
    buffered: Vec<super::AudioChunk>,
    collecting: Option<bool>,
    released: Option<Instant>,
    announced: HashMap<String, bool>,
    release_confirmed: bool,
    authorized: HashSet<String>,
    held_finals: HashMap<String, super::AudioChunk>,
    aborted: HashSet<String>,
    interrupted_at: Option<Instant>,
    require_press: bool,
}
impl Interaction {
    pub fn state(&self) -> State {
        self.state
    }
    pub fn accepts_taps(&self) -> bool {
        self.state == State::Idle
    }
    pub fn button(&mut self, pressed: bool, now: Instant) {
        if pressed {
            self.pressed_since.get_or_insert(now);
        } else {
            self.pressed_since = None;
        }
    }
    fn activity(&mut self, key: &str, collecting: bool, events: &mut Vec<InputEvent>) {
        if self.announced.insert(key.to_owned(), collecting) != Some(collecting) {
            events.push(InputEvent::Activity {
                key: key.to_owned(),
                collecting,
            });
        }
    }
    /// Debounced device state controls the indicator, independently of audio EOF.
    pub fn observe_state(
        &mut self,
        pressed: bool,
        now: Instant,
        grace: Duration,
    ) -> Vec<InputEvent> {
        let previous = self.collecting.replace(pressed);
        if self.interrupted_at.is_some() {
            return vec![];
        }
        if pressed && previous == Some(false) {
            self.require_press = false;
        }
        if pressed && previous != Some(true) {
            self.release_confirmed = false;
        }
        if !pressed && previous == Some(true) {
            self.release_confirmed = true;
        }
        let resume = pressed
            && previous != Some(true)
            && self.released.is_some_and(|t| now.duration_since(t) < grace);
        if !pressed && previous != Some(false) {
            self.released = Some(now);
        }
        let mut events = vec![];
        if !pressed || resume {
            for key in self.recordings.clone() {
                self.activity(&key, pressed, &mut events);
                if pressed {
                    self.authorized.remove(&key);
                } else if self.release_confirmed {
                    self.authorized.insert(key);
                }
            }
            if !self.recordings.is_empty() {
                self.state = if pressed {
                    State::Recording
                } else {
                    State::Dictating
                };
            }
        }
        if !pressed && self.release_confirmed {
            let finals = std::mem::take(&mut self.held_finals);
            for (key, part) in finals {
                self.authorized.insert(key);
                events.extend(self.apply(vec![InputEvent::Audio(part)]));
            }
        }
        events
    }
    fn activate(&mut self, recording: bool) -> Vec<InputEvent> {
        let mut events = vec![];
        for part in std::mem::take(&mut self.buffered) {
            self.recordings.insert(part.key.clone());
            if !recording && self.release_confirmed {
                self.authorized.insert(part.key.clone());
            }
            self.activity(&part.key, recording, &mut events);
            events.push(InputEvent::Audio(part));
        }
        if !events.is_empty() {
            self.state = if recording {
                State::Recording
            } else {
                State::Dictating
            };
        }
        events
    }
    pub fn poll(&mut self, now: Instant) -> Vec<InputEvent> {
        if let Some(start) = self.interrupted_at {
            if now.duration_since(start) < Duration::from_secs(5) {
                return vec![];
            }
            self.interrupted_at = None;
            let keys = std::mem::take(&mut self.recordings);
            self.buffered.clear();
            self.held_finals.clear();
            self.authorized.clear();
            self.state = State::Idle;
            for key in &keys {
                self.announced.remove(key);
            }
            return keys.into_iter().map(InputEvent::Cancel).collect();
        }
        // A receiver state can outlive a short tap while BLE catches up. Require
        // actual audio as well, so an empty tap never opens a placeholder panel.
        if self
            .pressed_since
            .is_some_and(|start| now.duration_since(start) >= HOLD_THRESHOLD)
        {
            return self.activate(self.collecting != Some(false) && self.state != State::Error);
        }
        vec![]
    }
    pub fn complete(&mut self, key: &str) {
        self.pending.remove(key);
        self.announced.remove(key);
        self.authorized.remove(key);
        self.settle();
    }
    fn settle(&mut self) {
        if self.interrupted_at.is_some() {
            self.state = State::Error;
            return;
        }
        if self.recordings.is_empty() && self.buffered.is_empty() {
            self.state = if self.pending.is_empty() {
                State::Idle
            } else {
                State::Dictating
            };
        }
    }
    pub fn disconnected(&mut self) -> Vec<InputEvent> {
        self.pressed_since = None;
        self.collecting = None;
        self.release_confirmed = false;
        self.require_press = true;
        self.interrupted_at.get_or_insert_with(Instant::now);
        self.aborted.extend(self.recordings.iter().cloned());
        self.aborted
            .extend(self.buffered.iter().map(|p| p.key.clone()));
        self.buffered.clear();
        self.state = State::Error;
        self.recordings
            .iter()
            .cloned()
            .map(InputEvent::Interrupted)
            .collect()
    }
    pub fn ready(&mut self) {
        // Connectivity is not a release or permission to recognize old audio.
    }

    pub fn apply(&mut self, events: Vec<InputEvent>) -> Vec<InputEvent> {
        let mut result = vec![];
        for event in events {
            match event {
                InputEvent::State(_) => {} // Continuation/EOF cannot infer physical release.
                InputEvent::Audio(part) => {
                    if self.interrupted_at.is_some()
                        || self.require_press
                        || self.aborted.contains(&part.key)
                    {
                        self.aborted.insert(part.key.clone());
                        if part.final_part {
                            self.aborted.remove(&part.key);
                        }
                        if part.final_part
                            && let Some(checkpoint) = part.checkpoint
                        {
                            result.push(InputEvent::Checkpoint(checkpoint));
                        }
                        continue;
                    }
                    let known = self.recordings.contains(&part.key);
                    if !known && !part.final_part {
                        if !part.samples.is_empty() {
                            self.buffered.push(part);
                            let seconds: f64 = self
                                .buffered
                                .iter()
                                .map(|p| p.samples.len() as f64 / p.rate.max(1) as f64)
                                .sum();
                            // Audio duration is a fallback when a press edge was
                            // missed; receipt/transfer delay is not hold duration.
                            if seconds >= HOLD_THRESHOLD.as_secs_f64() {
                                result.extend(self.activate(
                                    self.collecting != Some(false) && self.state != State::Error,
                                ));
                            }
                        }
                        continue;
                    }
                    if part.final_part {
                        result.extend(self.activate(!self.release_confirmed));
                        if !self.recordings.contains(&part.key) && part.samples.is_empty() {
                            if let Some(checkpoint) = part.checkpoint {
                                result.push(InputEvent::Checkpoint(checkpoint));
                            }
                            self.settle();
                            continue;
                        }
                        if !self.authorized.contains(&part.key) {
                            if self.release_confirmed {
                                self.authorized.insert(part.key.clone());
                            } else {
                                self.held_finals.insert(part.key.clone(), part);
                                continue;
                            }
                        }
                        self.activity(&part.key, false, &mut result);
                        self.recordings.remove(&part.key);
                        self.pending.insert(part.key.clone());
                        if self.recordings.is_empty() {
                            self.state = State::Dictating;
                        }
                    }
                    result.push(InputEvent::Audio(part));
                }
                InputEvent::Discard(key) => {
                    self.buffered.retain(|part| part.key != key);
                    self.pending.remove(&key);
                    self.announced.remove(&key);
                    if self.recordings.remove(&key) {
                        result.push(InputEvent::Discard(key));
                    }
                    self.settle();
                }
                other => result.push(other),
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::input::AudioChunk;
    use serde_json::json;
    fn audio(key: &str, samples: Vec<i16>, final_part: bool) -> InputEvent {
        InputEvent::Audio(AudioChunk {
            key: key.into(),
            samples,
            rate: 16000,
            final_part,
            checkpoint: final_part.then(|| json!(12)),
        })
    }
    #[test]
    fn recording_starts_at_150ms_of_audio_without_waiting_for_a_second_large_chunk() {
        let mut gate = Interaction::default();
        assert!(
            gate.apply(vec![audio("hold", vec![1; 2399], false)])
                .is_empty()
        );
        let events = gate.apply(vec![audio("hold", vec![1], false)]);
        assert!(matches!(
            events.first(),
            Some(InputEvent::Activity {
                collecting: true,
                ..
            })
        ));
        let samples: usize = events
            .iter()
            .map(|event| match event {
                InputEvent::Audio(p) => p.samples.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(samples, 2400);
    }
    #[test]
    fn resumed_press_requires_its_own_release_before_dictation() {
        let mut gate = Interaction::default();
        let now = Instant::now();
        let grace = Duration::from_millis(500);
        gate.observe_state(true, now, grace);
        gate.apply(vec![audio("resume", vec![1; 5600], false)]);
        gate.observe_state(false, now + Duration::from_secs(1), grace);
        gate.observe_state(true, now + Duration::from_millis(1200), grace);
        assert_eq!(gate.state(), State::Recording);
        assert!(
            gate.apply(vec![audio("resume", vec![1; 100], true)])
                .is_empty()
        );
        let end = gate.observe_state(false, now + Duration::from_secs(2), grace);
        assert!(matches!(end.last(), Some(InputEvent::Audio(p)) if p.final_part));
        assert_eq!(gate.state(), State::Dictating);
    }
    #[test]
    fn audio_eof_cannot_start_dictation_without_a_release_edge() {
        let mut gate = Interaction::default();
        let now = Instant::now();
        gate.observe_state(true, now, Duration::from_millis(500));
        gate.apply(vec![audio("held", vec![1; 5600], false)]);
        let eof = gate.apply(vec![
            InputEvent::State(false),
            audio("held", vec![1; 200], true),
        ]);
        assert!(eof.is_empty());
        assert_eq!(gate.state(), State::Recording);
        let release = gate.observe_state(
            false,
            now + Duration::from_secs(1),
            Duration::from_millis(500),
        );
        assert!(
            matches!(release.as_slice(), [InputEvent::Activity { collecting: false, .. }, InputEvent::Audio(p)] if p.final_part && p.samples.len() == 200)
        );
        assert_eq!(gate.state(), State::Dictating);
    }
    #[test]
    fn release_stops_indicator_before_delayed_eof_and_recovery_never_restarts_it() {
        let now = Instant::now();
        let mut state = Interaction::default();
        let grace = Duration::from_millis(500);
        state.observe_state(true, now, grace);
        let begin = state.apply(vec![audio("2351", vec![1; 5600], false)]);
        assert!(
            matches!(&begin[0], InputEvent::Activity { key, collecting: true } if key == "2351")
        );
        // Replay the latest log: release at 17:22:27.597, disconnect at
        // 17:22:29.024, last audio at 17:22:50.578.
        let release = state.observe_state(false, now + Duration::from_secs(31), grace);
        assert!(matches!(
            release.as_slice(),
            [InputEvent::Activity {
                collecting: false,
                ..
            }]
        ));
        assert_eq!(state.state(), State::Dictating);
        assert!(matches!(
            state.disconnected().as_slice(),
            [InputEvent::Interrupted(_)]
        ));
        let interrupted = state.interrupted_at.unwrap();
        assert!(
            state
                .poll(interrupted + Duration::from_millis(4999))
                .is_empty()
        );
        for _ in 0..53 {
            let events = state.apply(vec![audio("2351", vec![2; 2400], false)]);
            assert!(events.is_empty());
        }
        let end = state.apply(vec![InputEvent::State(false), audio("2351", vec![], true)]);
        assert!(end.iter().all(|e| !matches!(
            e,
            InputEvent::Activity {
                collecting: true,
                ..
            }
        )));
        assert!(end.iter().all(|e| matches!(e, InputEvent::Checkpoint(_))));
        assert!(
            matches!(state.poll(interrupted + Duration::from_secs(5)).as_slice(), [InputEvent::Cancel(key)] if key == "2351")
        );
        assert_eq!(state.state(), State::Idle);
    }
    #[test]
    fn resume_updates_same_identity_but_late_press_does_not_revive_old_recording() {
        let now = Instant::now();
        let grace = Duration::from_millis(500);
        let mut state = Interaction::default();
        state.observe_state(true, now, grace);
        state.apply(vec![audio("a", vec![1; 5600], false)]);
        state.observe_state(false, now + Duration::from_secs(1), grace);
        let resumed = state.observe_state(true, now + Duration::from_millis(1300), grace);
        assert!(
            matches!(resumed.as_slice(), [InputEvent::Activity { key, collecting: true }] if key == "a")
        );
        state.observe_state(false, now + Duration::from_secs(2), grace);
        assert!(
            state
                .observe_state(true, now + Duration::from_secs(3), grace)
                .is_empty()
        );
        assert_eq!(state.state(), State::Dictating);
    }
    #[test]
    fn first_audio_arriving_after_release_starts_in_receiving_state() {
        let mut state = Interaction::default();
        state.observe_state(false, Instant::now(), Duration::from_millis(500));
        let events = state.apply(vec![audio("late", vec![1; 5600], false)]);
        assert!(matches!(
            &events[0],
            InputEvent::Activity {
                collecting: false,
                ..
            }
        ));
        assert_eq!(state.state(), State::Dictating);
    }
    #[test]
    fn unclassified_press_and_empty_tap_never_create_a_recording() {
        let mut gate = Interaction::default();
        assert!(
            gate.apply(vec![
                InputEvent::State(true),
                audio("tap", vec![], false),
                InputEvent::State(false)
            ])
            .is_empty()
        );
        let events = gate.apply(vec![audio("tap", vec![], true)]);
        assert!(matches!(events.as_slice(), [InputEvent::Checkpoint(v)] if *v == json!(12)));
    }
    #[test]
    fn double_tap_stays_hidden_and_keeps_checkpoint_order() {
        let mut gate = Interaction::default();
        let events = gate.apply(vec![
            InputEvent::State(true),
            audio("tap1", vec![], true),
            InputEvent::State(false),
            InputEvent::State(true),
            audio("tap2", vec![], true),
            InputEvent::State(false),
        ]);
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|e| matches!(e, InputEvent::Checkpoint(_)))
        );
    }
    #[test]
    fn tap_then_hold_starts_once_without_dropping_initial_pcm() {
        let mut gate = Interaction::default();
        assert!(
            gate.apply(vec![
                InputEvent::State(true),
                audio("joined", vec![], false)
            ])
            .is_empty()
        );
        let now = Instant::now();
        gate.button(true, now);
        gate.observe_state(true, now, Duration::from_millis(500));
        assert!(
            gate.apply(vec![audio("joined", vec![0; 100], false)])
                .is_empty()
        );
        assert!(gate.poll(now + Duration::from_millis(149)).is_empty());
        let events = gate.poll(now + Duration::from_millis(150));
        assert!(matches!(
            events.first(),
            Some(InputEvent::Activity {
                collecting: true,
                ..
            })
        ));
        assert!(matches!(&events[1], InputEvent::Audio(p) if p.samples.len() == 100));
        gate.observe_state(
            false,
            now + Duration::from_secs(1),
            Duration::from_millis(500),
        );
        let next = gate.apply(vec![
            InputEvent::State(true),
            audio("joined", vec![1; 100], false),
            InputEvent::State(false),
            audio("joined", vec![], true),
        ]);
        assert!(matches!(next.last(), Some(InputEvent::Audio(p)) if p.final_part));
        assert!(gate.recordings.is_empty());
        assert_eq!(gate.state, State::Dictating);
        gate.complete("joined");
        assert_eq!(gate.state, State::Idle);
    }
    #[test]
    fn completed_real_audio_is_not_treated_as_a_tap() {
        let mut gate = Interaction::default();
        let now = Instant::now();
        gate.observe_state(true, now, Duration::from_millis(500));
        gate.observe_state(
            false,
            now + Duration::from_secs(1),
            Duration::from_millis(500),
        );
        assert!(
            matches!(gate.apply(vec![audio("held", vec![0; 5000], true)]).as_slice(), [InputEvent::Activity { collecting: false, .. }, InputEvent::Audio(p)] if p.samples.len() == 5000)
        );
    }
}
