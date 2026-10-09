//! In-process ring input for applications that do not run the dictation
//! worker: Bluetooth reception and the gesture reducer run on tokio tasks and
//! surface as session-scoped events with PCM attached.
use crate::{
    capture::{CaptureOptions, receive},
    input::{self, InputAdapter, InputEvent, RingInputConfig, gesture_types::GestureEvent},
    reception::{
        input_effects::Effect,
        session_state::{Action, SessionId},
    },
};
use anyhow::{Context, Result};
use pebble_core::{ipc, output::Output, pcm::Pcm};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet};

#[derive(Clone, Debug)]
pub enum RingEvent {
    /// The button went down for a new session.
    PressStarted {
        session: SessionId,
    },
    /// The button came up. Audio for the session can still arrive afterwards.
    PressEnded {
        session: SessionId,
    },
    /// New PCM since the previous `Audio` of the same generation.
    Audio {
        session: SessionId,
        generation: u64,
        pcm: Pcm,
        rate: u32,
    },
    /// Live audio restarts from sample 0 with a new generation.
    LiveReset {
        session: SessionId,
        generation: u64,
    },
    /// No more live audio; without a following `Recording` the press was a tap.
    LiveStopped {
        session: SessionId,
    },
    /// The complete recording from sample 0, available once every part is final.
    Recording {
        session: SessionId,
        generation: u64,
        pcm: Pcm,
        rate: u32,
    },
    Gesture(GestureEvent),
    Level {
        session: SessionId,
        level: f64,
    },
    Connected(bool),
    Error(String),
}

/// Translates `InputEvent`s into `RingEvent`s and remembers which sessions
/// are collecting so press edges are reported once.
#[derive(Default)]
pub struct EventMapper {
    namespace: Option<String>,
    collecting: HashMap<SessionId, bool>,
    connected: Option<bool>,
}
impl EventMapper {
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
    pub fn key(&self, session: SessionId) -> Option<String> {
        self.namespace.as_ref().map(|n| format!("{n}-{session}"))
    }
    fn session(&self, key: &str) -> Option<SessionId> {
        key.strip_prefix(&format!("{}-", self.namespace.as_ref()?))?
            .parse()
            .ok()
    }
    pub fn map(&mut self, event: InputEvent) -> Vec<RingEvent> {
        match event {
            InputEvent::Gesture { event, .. } => vec![RingEvent::Gesture(event)],
            InputEvent::Level { key, level } => self
                .session(&key)
                .map(|session| vec![RingEvent::Level { session, level }])
                .unwrap_or_default(),
            InputEvent::Reception { namespace, effect } => {
                if self.namespace.is_none() {
                    self.namespace = Some(namespace);
                }
                self.effect(effect)
            }
            InputEvent::Audio(_)
            | InputEvent::State(_)
            | InputEvent::Discard(_)
            | InputEvent::Flush
            | InputEvent::Checkpoint(_) => vec![],
        }
    }
    fn effect(&mut self, effect: Effect) -> Vec<RingEvent> {
        match effect {
            Effect::Snapshot(snapshot) => {
                if self.connected != Some(snapshot.connected) {
                    self.connected = Some(snapshot.connected);
                    vec![RingEvent::Connected(snapshot.connected)]
                } else {
                    vec![]
                }
            }
            Effect::View(view) => {
                if view.cancelled || view.failed {
                    return vec![];
                }
                match self.collecting.insert(view.id, view.collecting) {
                    None if view.collecting => vec![RingEvent::PressStarted { session: view.id }],
                    Some(true) if !view.collecting => {
                        vec![RingEvent::PressEnded { session: view.id }]
                    }
                    _ => vec![],
                }
            }
            Effect::ResetLive {
                session,
                generation,
            } => vec![RingEvent::LiveReset {
                session,
                generation,
            }],
            Effect::Live(plan) => vec![RingEvent::Audio {
                session: plan.session,
                generation: plan.generation,
                pcm: plan.pcm,
                rate: plan.rate,
            }],
            Effect::StopLive(session) => vec![RingEvent::LiveStopped { session }],
            Effect::Batch(plan) => vec![RingEvent::Recording {
                session: plan.session,
                generation: plan.generation,
                pcm: plan.pcm,
                rate: plan.rate,
            }],
            Effect::Action(Action::Retire { session, .. }) => {
                self.collecting.remove(&session);
                vec![]
            }
            Effect::Action(_) => vec![],
        }
    }
}

/// Running ring input. Dropping it stops reception.
pub struct RingInput {
    adapter: Arc<Mutex<Box<dyn InputAdapter>>>,
    mapper: Arc<Mutex<EventMapper>>,
    _tasks: JoinSet<()>,
}
impl RingInput {
    pub async fn start(
        options: CaptureOptions,
        config: RingInputConfig,
        output: Output,
    ) -> Result<(Self, mpsc::UnboundedReceiver<RingEvent>)> {
        let adapter: Arc<Mutex<Box<dyn InputAdapter>>> = Arc::new(Mutex::new(input::create(
            &options.address,
            "listen",
            config,
        )?));
        let mapper = Arc::new(Mutex::new(EventMapper::default()));
        let (wire_tx, mut wire_rx) = ipc::process_channel::<Value>("ring reception");
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let mut tasks = JoinSet::new();
        {
            let event_tx = event_tx.clone();
            let output = output.clone();
            tasks.spawn(async move {
                if let Err(error) = receive(options, Some(wire_tx), output).await {
                    let _ = event_tx.send(RingEvent::Error(format!("{error:#}")));
                }
            });
        }
        {
            let adapter = adapter.clone();
            let mapper = mapper.clone();
            tasks.spawn(async move {
                let result = async {
                    let mut clock = tokio::time::interval(Duration::from_millis(5));
                    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    loop {
                        let events = tokio::select! {
                            biased;
                            message = wire_rx.recv() => {
                                let message = message?.context("Reception ended")?;
                                adapter.lock().unwrap().decode(message, &output)?
                            }
                            _ = clock.tick() => adapter.lock().unwrap().poll(&output)?,
                        };
                        for event in events {
                            for mapped in mapper.lock().unwrap().map(event) {
                                if event_tx.send(mapped).is_err() {
                                    return Ok::<(), anyhow::Error>(());
                                }
                            }
                        }
                        tokio::task::yield_now().await;
                    }
                }
                .await;
                if let Err(error) = result {
                    let _ = event_tx.send(RingEvent::Error(format!("{error:#}")));
                }
            });
        }
        Ok((
            Self {
                adapter,
                mapper,
                _tasks: tasks,
            },
            event_rx,
        ))
    }
    /// Acknowledge a `Recording` so the receive store releases its audio.
    pub fn completed(&self, session: SessionId) {
        if let Some(key) = self.mapper.lock().unwrap().key(session) {
            self.adapter.lock().unwrap().completed(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reception::{
        config::Reception,
        input_effects::AudioPlan,
        session_state::{AppState, GestureState, SessionView, Snapshot},
    };

    fn view(id: SessionId, collecting: bool) -> InputEvent {
        InputEvent::Reception {
            namespace: "ring-x".into(),
            effect: Effect::View(SessionView {
                id,
                generation: 1,
                sources: vec![],
                visible: true,
                collecting,
                live: true,
                dictating: false,
                failed: false,
                cancelled: false,
                policy: Reception::default(),
            }),
        }
    }
    fn kinds(events: &[RingEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e {
                RingEvent::PressStarted { .. } => "start",
                RingEvent::PressEnded { .. } => "end",
                RingEvent::Audio { .. } => "audio",
                RingEvent::LiveReset { .. } => "reset",
                RingEvent::LiveStopped { .. } => "stop",
                RingEvent::Recording { .. } => "recording",
                RingEvent::Gesture(_) => "gesture",
                RingEvent::Level { .. } => "level",
                RingEvent::Connected(_) => "connected",
                RingEvent::Error(_) => "error",
            })
            .collect()
    }

    #[test]
    fn press_edges_are_reported_once_per_session() {
        let mut mapper = EventMapper::default();
        assert_eq!(kinds(&mapper.map(view(1, true))), ["start"]);
        assert_eq!(kinds(&mapper.map(view(1, true))), [] as [&str; 0]);
        assert_eq!(kinds(&mapper.map(view(1, false))), ["end"]);
        assert_eq!(kinds(&mapper.map(view(1, false))), [] as [&str; 0]);
        assert_eq!(kinds(&mapper.map(view(2, true))), ["start"]);
    }

    #[test]
    fn audio_levels_and_recordings_carry_the_session() {
        let mut mapper = EventMapper::default();
        mapper.map(view(7, true));
        let level = mapper.map(InputEvent::Level {
            key: "ring-x-7".into(),
            level: 0.5,
        });
        assert!(matches!(level[0], RingEvent::Level { session: 7, .. }));
        let batch = mapper.map(InputEvent::Reception {
            namespace: "ring-x".into(),
            effect: Effect::Batch(AudioPlan {
                session: 7,
                generation: 3,
                start_sample: 0,
                rate: 9997,
                pcm: vec![1i16, 2, 3].into(),
            }),
        });
        let RingEvent::Recording {
            session,
            generation,
            pcm,
            rate,
        } = &batch[0]
        else {
            panic!()
        };
        assert_eq!((*session, *generation, pcm.len(), *rate), (7, 3, 3, 9997));
        assert_eq!(mapper.key(7).as_deref(), Some("ring-x-7"));
    }

    #[test]
    fn connection_changes_are_reported_on_change_only() {
        let mut mapper = EventMapper::default();
        let snapshot = |connected| InputEvent::Reception {
            namespace: "ring-x".into(),
            effect: Effect::Snapshot(Snapshot {
                session_id: None,
                gesture_state: GestureState::Idle,
                prefix: None,
                last_completed_gesture: None,
                app_state: AppState::Idle,
                collecting: false,
                connected,
                ui_deadline: None,
                resume_deadline: None,
                tap_deadline: None,
                generation: 0,
            }),
        };
        assert_eq!(kinds(&mapper.map(snapshot(true))), ["connected"]);
        assert_eq!(kinds(&mapper.map(snapshot(true))), [] as [&str; 0]);
        assert_eq!(kinds(&mapper.map(snapshot(false))), ["connected"]);
    }
}
