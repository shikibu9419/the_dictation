//! Runtime bridge between the pure reducer, receive-owned PCM and output hooks.
use super::{
    InputEvent,
    gesture_types::{Gesture, GestureEvent},
    gestures::{Hooks, LogHook},
};
use crate::{
    output::Output,
    recordings::{Checkpoint, Recordings},
    settings::Settings,
};
use anyhow::{Context, Result, ensure};
use pebble_index::reception::{
    input_effects::{Effect, InputEffects},
    session_state::{
        Action, Gesture as Completed, Observation, SessionId, SessionState, Transition,
    },
};
use std::collections::HashMap;

pub struct Interaction {
    machine: SessionState,
    effects: InputEffects,
    hooks: Hooks,
    namespace: String,
    live_enabled: bool,
    // Release acknowledgement is per logical recording, never the latest press.
    checkpoints: HashMap<SessionId, Checkpoint>,
    last_snapshot: Option<pebble_index::reception::session_state::Snapshot>,
}
impl Interaction {
    pub fn new(settings: &Settings) -> Result<Self> {
        let mut hooks = Hooks::default();
        hooks.register(LogHook);
        Ok(Self {
            machine: SessionState::new(settings.reception)?,
            effects: InputEffects::default(),
            hooks,
            namespace: format!("ring-{}", uuid::Uuid::new_v4()),
            live_enabled: settings.presentation.live_mode,
            checkpoints: HashMap::new(),
            last_snapshot: None,
        })
    }
    pub fn key(&self, session: SessionId) -> String {
        format!("{}-{session}", self.namespace)
    }
    pub fn id(&self, key: &str) -> Option<SessionId> {
        key.strip_prefix(&format!("{}-", self.namespace))?
            .parse()
            .ok()
    }
    pub fn observe(
        &mut self,
        time: u64,
        observation: Observation,
        store: &mut Recordings,
        output: &Output,
    ) -> Result<Vec<InputEvent>> {
        output.debug(format!("Reception observation t={time}ms: {observation:?}"));
        let transition = self.machine.observe(time, observation)?;
        self.apply(transition, store, output)
    }
    pub fn tick(
        &mut self,
        time: u64,
        store: &mut Recordings,
        output: &Output,
    ) -> Result<Vec<InputEvent>> {
        let transition = self.machine.tick(time)?;
        self.apply(transition, store, output)
    }
    fn apply(
        &mut self,
        transition: Transition,
        store: &mut Recordings,
        output: &Output,
    ) -> Result<Vec<InputEvent>> {
        let effects = self
            .effects
            .reconcile(transition, store, self.live_enabled)?;
        let mut result = vec![];
        for effect in effects {
            if let Effect::Snapshot(snapshot) = &effect {
                if self.last_snapshot.as_ref() == Some(snapshot) {
                    continue;
                }
                output.debug(format!("Reception snapshot: {snapshot:?}"));
                self.last_snapshot = Some(snapshot.clone());
            }
            if let Effect::Action(action) = &effect {
                output.debug(format!("Reception action: {action:?}"));
                match action {
                    Action::MergeSession { .. } => {}
                    Action::Recognize { session, sources } => {
                        let last = sources
                            .last()
                            .context("Cannot recognize a recording without sources")?;
                        self.checkpoints.insert(*session, store.checkpoint(last)?);
                    }
                    Action::Retire { session, sources } => {
                        let checkpoint = self
                            .checkpoints
                            .remove(session)
                            .or_else(|| sources.last().and_then(|s| store.checkpoint(s).ok()));
                        for source in sources {
                            store.release(source);
                        }
                        if let Some(checkpoint) = checkpoint {
                            result.push(InputEvent::Checkpoint(serde_json::json!(checkpoint)));
                        }
                    }
                    Action::Gesture(event) => {
                        let gesture = match event.gesture {
                            Completed::SinglePush => Some(Gesture::SingleTap),
                            Completed::DoublePush => Some(Gesture::DoubleTap),
                            _ => None,
                        };
                        if let Some(gesture) = gesture {
                            let event = GestureEvent {
                                gesture,
                                first_collection: Some(event.first_collection as u16),
                                last_collection: Some(event.last_collection as u16),
                            };
                            self.hooks.dispatch(event, output);
                            result.push(InputEvent::Gesture(event));
                        }
                    }
                    Action::Ambiguous { .. } => {}
                }
            }
            result.push(InputEvent::Reception {
                namespace: self.namespace.clone(),
                effect,
            });
        }
        Ok(result)
    }
    pub fn source_session(&self, source: &str) -> Option<SessionId> {
        self.machine.source_session(source)
    }
    pub fn meter_key(&self, first: u64) -> Option<String> {
        self.machine.meter_session(first).map(|id| self.key(id))
    }
    pub fn ensure_flushed(&self) -> Result<()> {
        ensure!(
            !self.machine.waiting_for_input(),
            "Input ended before final audio or gesture deadline: {:?}",
            self.machine.snapshot()
        );
        Ok(())
    }
}
