//! Pure reception reducer. Times are monotonic milliseconds supplied by the
//! receiver. Inputs at a deadline must be drained before `tick(deadline)`.
//! No PCM, process, window, or physical-button timing is owned here.
use super::{button_detector::Press, config::Reception};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

pub type SessionId = u64;
pub type SourceId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureState {
    Idle,
    Holding,
    SinglePending,
    ReleasePending,
    ResumePending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppState {
    Idle,
    Recording,
    Dictating,
    Error,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gesture {
    SinglePush,
    DoublePush,
    LongHold,
    SingleThenHold,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompletedGesture {
    pub id: u64,
    pub gesture: Gesture,
    pub first_collection: u64,
    pub last_collection: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub session_id: Option<SessionId>,
    pub gesture_state: GestureState,
    pub prefix: Option<Gesture>,
    pub last_completed_gesture: Option<CompletedGesture>,
    pub app_state: AppState,
    pub collecting: bool,
    pub connected: bool,
    pub ui_deadline: Option<u64>,
    pub resume_deadline: Option<u64>,
    pub tap_deadline: Option<u64>,
    pub generation: u64,
}

/// Source observations are emitted even for metadata-only/empty-final records.
/// `complete` means the store has final AND every preceding collection.
#[derive(Clone, Debug)]
pub struct SourceObservation {
    pub id: SourceId,
    pub first_collection: u64,
    pub last_collection: u64,
    pub classification: Option<Press>,
    pub final_seen: bool,
    pub complete: bool,
}
#[derive(Clone, Debug)]
pub enum Observation {
    /// `unread` is the oldest not-yet-decoded collection at this S observation.
    /// It bounds association of subsequently decoded, previously unknown sources.
    Collecting {
        active: bool,
        unread: u64,
    },
    Source(SourceObservation),
    /// End positions are exclusive and extended across wire counter wrap.
    Watermark {
        known_end: u64,
        processed_end: u64,
    },
    /// S's low-byte count changed, but the full R response is not available.
    /// A false observation follows Watermark from that R. Never infer a full
    /// range (or a gesture) from the low-byte count alone.
    RangePending(bool),
    Connected(bool),
    Recognized(SessionId),
    Lost(SourceId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    MergeSession {
        from: SessionId,
        into: SessionId,
    },
    /// Exactly once, only after release grace AND all sources are complete.
    Recognize {
        session: SessionId,
        sources: Vec<SourceId>,
    },
    /// A consumer acknowledgement or confirmed tap allows source-store release.
    Retire {
        session: SessionId,
        sources: Vec<SourceId>,
    },
    Gesture(CompletedGesture),
    Ambiguous {
        source: SourceId,
        reason: &'static str,
    },
}
/// An effects executor reconciles these desired views against its PCM cursors.
/// Provisional long continuations use the parent's live ID. If later rejected,
/// the source list shrinks and the executor must rebuild that live context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub id: SessionId,
    pub generation: u64,
    pub sources: Vec<SourceId>,
    pub visible: bool,
    pub collecting: bool,
    pub live: bool,
    pub dictating: bool,
    pub failed: bool,
    pub policy: Reception,
}
#[derive(Clone, Debug)]
pub struct Transition {
    pub snapshot: Snapshot,
    pub sessions: Vec<SessionView>,
    pub actions: Vec<Action>,
}
#[derive(Clone, Debug)]
struct Source {
    observation: SourceObservation,
    session: SessionId,
}
#[derive(Clone, Debug)]
struct Tap {
    first: u64,
    last: u64,
    deadline: u64,
    // Fixed at the first deadline check. Later arrivals cannot extend this fence.
    watermark: Option<u64>,
    waiting_for_range: bool,
}
impl Tap {
    fn accepts(&self, now: u64, collection: Option<u64>) -> bool {
        now <= self.deadline
            || collection.is_some_and(|c| self.watermark.is_some_and(|end| c < end))
    }
}
#[derive(Clone, Copy, Debug)]
struct Resume {
    parent: SessionId,
    within_grace: bool,
}
#[derive(Clone, Debug)]
struct Session {
    id: SessionId,
    generation: u64,
    first: u64,
    upper: Option<u64>,
    policy: Reception,
    sources: Vec<SourceId>,
    collecting: bool,
    release_at: Option<u64>,
    ui_deadline: Option<u64>,
    shown: bool,
    submitted: bool,
    failed: bool,
    prefix: Option<Tap>,
    resume: Option<Resume>,
}
impl Session {
    fn deadline(&self) -> Option<u64> {
        self.release_at
            .map(|t| t.saturating_add(self.policy.long_resume_grace_ms))
    }
    fn end(&mut self, now: u64) {
        self.collecting = false;
        self.ui_deadline = None;
        self.release_at.get_or_insert(now);
    }
}

pub struct SessionState {
    defaults: Reception,
    sessions: BTreeMap<SessionId, Session>,
    sources: HashMap<SourceId, Source>,
    active: Option<SessionId>,
    pending_tap: Option<Tap>,
    last_gesture: Option<CompletedGesture>,
    next_id: u64,
    next_gesture: u64,
    generation: u64,
    connected: bool,
    now: u64,
    known_end: u64,
    processed_end: u64,
    range_pending: bool,
}
impl Default for SessionState {
    fn default() -> Self {
        Self::new(Reception::default()).expect("valid default policy")
    }
}
impl SessionState {
    pub fn new(defaults: Reception) -> Result<Self> {
        defaults.validate()?;
        Ok(Self {
            defaults,
            sessions: BTreeMap::new(),
            sources: HashMap::new(),
            active: None,
            pending_tap: None,
            last_gesture: None,
            next_id: 0,
            next_gesture: 0,
            generation: 0,
            connected: true,
            now: 0,
            known_end: 0,
            processed_end: 0,
            range_pending: false,
        })
    }
    pub fn configure(&mut self, defaults: Reception) -> Result<()> {
        defaults.validate()?;
        self.defaults = defaults;
        Ok(())
    }
    pub fn observe(&mut self, now: u64, observation: Observation) -> Result<Transition> {
        ensure!(
            now >= self.now,
            "Reception observations arrived out of monotonic order"
        );
        self.validate_observation(&observation)?;
        self.now = now;
        let mut actions = vec![];
        // Strictly earlier deadlines run first; equal-time input always wins.
        self.timers(now, false, &mut actions);
        match observation {
            Observation::Collecting { active, unread } => self.collecting(active, unread, now)?,
            Observation::Source(source) => self.source(source, now, &mut actions)?,
            Observation::Watermark {
                known_end,
                processed_end,
            } => {
                ensure!(
                    processed_end <= known_end,
                    "Decoded watermark exceeds known receive range"
                );
                self.known_end = self.known_end.max(known_end);
                self.processed_end = self.processed_end.max(processed_end);
            }
            Observation::RangePending(pending) => {
                self.range_pending = pending;
                if !pending
                    && let Some(tap) = &mut self.pending_tap
                    && tap.waiting_for_range
                {
                    // Freeze once. A later S/R cannot keep extending this tap.
                    tap.waiting_for_range = false;
                    tap.watermark = Some(self.known_end);
                }
            }
            Observation::Connected(connected) => self.connected = connected,
            Observation::Recognized(id) => self.retire(id, &mut actions),
            Observation::Lost(id) => {
                if let Some(source) = self.sources.get(&id) {
                    let session = self.sessions.get_mut(&source.session).unwrap();
                    session.failed = true;
                    session.collecting = false;
                    session.ui_deadline = None;
                }
            }
        }
        self.settle(now, &mut actions);
        Ok(self.transition(actions))
    }
    pub fn tick(&mut self, now: u64) -> Result<Transition> {
        ensure!(now >= self.now, "Reception timer moved backwards");
        self.now = now;
        let mut actions = vec![];
        self.timers(now, true, &mut actions);
        self.settle(now, &mut actions);
        Ok(self.transition(actions))
    }
    pub fn snapshot(&self) -> Snapshot {
        self.transition(vec![]).snapshot
    }
    pub fn waiting_for_input(&self) -> bool {
        self.pending_tap.is_some() || self.sessions.values().any(|s| !s.submitted)
    }
    pub fn source_session(&self, source: &str) -> Option<SessionId> {
        let owner = self.sources.get(source)?.session;
        Some(
            self.sessions[&owner]
                .resume
                .filter(|r| r.within_grace)
                .map_or(owner, |r| r.parent),
        )
    }

    fn validate_observation(&self, observation: &Observation) -> Result<()> {
        match observation {
            Observation::Watermark {
                known_end,
                processed_end,
            } => {
                ensure!(
                    processed_end <= known_end,
                    "Decoded watermark exceeds known receive range"
                );
            }
            Observation::Source(incoming) => {
                ensure!(
                    incoming.first_collection <= incoming.last_collection,
                    "Source collection interval is reversed"
                );
                ensure!(
                    !incoming.complete || incoming.final_seen,
                    "Complete source has no final marker"
                );
                if let Some(source) = self.sources.get(&incoming.id) {
                    let previous = &source.observation;
                    ensure!(
                        previous.first_collection == incoming.first_collection,
                        "Source origin changed"
                    );
                    ensure!(
                        previous.last_collection <= incoming.last_collection,
                        "Source progress regressed"
                    );
                    ensure!(
                        !previous.final_seen || incoming.final_seen,
                        "Source final marker regressed"
                    );
                    ensure!(
                        !previous.complete || incoming.complete,
                        "Source completeness regressed"
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn prepare(&mut self, first: u64, now: u64, collecting: bool) -> Result<SessionId> {
        ensure!(
            self.sessions.len() < 128,
            "Reception session limit reached; retained recordings require recovery"
        );
        self.next_id += 1;
        self.generation += 1;
        let id = self.next_id;
        self.sessions.insert(
            id,
            Session {
                id,
                generation: self.generation,
                first,
                upper: None,
                policy: self.defaults,
                sources: vec![],
                collecting,
                release_at: (!collecting).then_some(now),
                ui_deadline: collecting
                    .then_some(now.saturating_add(self.defaults.hold_ui_delay_ms)),
                shown: false,
                submitted: false,
                failed: false,
                prefix: None,
                resume: None,
            },
        );
        Ok(id)
    }
    fn collecting(&mut self, active: bool, unread: u64, now: u64) -> Result<()> {
        if !active {
            if let Some(session) = self.active.and_then(|id| self.sessions.get_mut(&id))
                && !session.submitted
                && !session.failed
            {
                session.end(now);
            }
            return Ok(());
        }
        if self
            .active
            .and_then(|id| self.sessions.get(&id))
            .is_some_and(|s| s.collecting)
        {
            return Ok(()); // Repeated S never extends the UI deadline.
        }
        let parent = self
            .active
            .and_then(|id| self.sessions.get(&id))
            .filter(|s| !s.submitted && !s.failed && s.release_at.is_some())
            .map(|s| Resume {
                parent: s.id,
                within_grace: s.deadline().is_some_and(|d| now <= d),
            });
        let id = self.prepare(unread, now, true)?;
        if let Some(parent) = parent {
            self.sessions.get_mut(&parent.parent).unwrap().upper = Some(unread);
        }
        let prefix = if self
            .pending_tap
            .as_ref()
            .is_some_and(|t| t.accepts(now, Some(unread)))
        {
            self.pending_tap.take()
        } else {
            None
        };
        let session = self.sessions.get_mut(&id).unwrap();
        session.prefix = prefix;
        session.resume = parent;
        self.active = Some(id);
        Ok(())
    }
    fn source(
        &mut self,
        incoming: SourceObservation,
        now: u64,
        actions: &mut Vec<Action>,
    ) -> Result<()> {
        let id = incoming.id.clone();
        let session_id = if let Some(source) = self.sources.get(&id) {
            source.session
        } else {
            let eligible: Vec<_> = self
                .sessions
                .values()
                .filter(|s| {
                    !s.submitted
                        && !s.failed
                        && s.sources.is_empty()
                        && incoming.first_collection >= s.first
                        && s.upper.is_none_or(|end| incoming.first_collection < end)
                })
                .map(|s| s.id)
                .collect();
            let session = if eligible.len() == 1 {
                eligible[0]
            } else {
                let session = self.prepare(incoming.first_collection, now, false)?;
                actions.push(Action::Ambiguous {
                    source: id.clone(),
                    reason: if eligible.is_empty() {
                        "no observed collecting candidate"
                    } else {
                        "overlapping candidate ranges"
                    },
                });
                session
            };
            self.sessions
                .get_mut(&session)
                .unwrap()
                .sources
                .push(id.clone());
            session
        };
        let mut incoming = incoming;
        let previous = self.sources.get(&id).map(|s| &s.observation);
        let continues_unfinished = previous.is_some_and(|s| {
            !s.final_seen && !incoming.final_seen && incoming.last_collection > s.last_collection
        });
        let newly_final = incoming.final_seen && previous.is_none_or(|s| !s.final_seen);
        if previous.is_some_and(|s| s.classification == Some(Press::Long)) {
            incoming.classification = Some(Press::Long);
        } else if incoming.classification.is_none() {
            incoming.classification = previous.and_then(|s| s.classification);
        }
        self.sources.insert(
            id,
            Source {
                observation: incoming,
                session: session_id,
            },
        );

        // A resumed S can arrive before its next C. A C already owned by an
        // unfinished source proves continuity, even after the 50ms merge grace.
        if let Some(child) = self.active.filter(|id| *id != session_id)
            && self.sessions.get(&child).is_some_and(|s| {
                s.sources.is_empty() && s.resume.is_some_and(|r| r.parent == session_id)
            })
            && continues_unfinished
        {
            self.join(session_id, child, actions);
        }
        if newly_final {
            self.sessions
                .get_mut(&session_id)
                .context("Source owner missing")?
                .end(now);
        }
        Ok(())
    }
    fn classification(&self, id: SessionId) -> Option<Press> {
        let session = self.sessions.get(&id)?;
        if session.sources.is_empty() {
            return None;
        }
        let mut result = None;
        for id in &session.sources {
            let press = self.sources[id].observation.classification?;
            if result.is_some_and(|old| old != press) {
                return None;
            }
            result = Some(press);
        }
        result
    }
    fn complete(&self, id: SessionId) -> bool {
        self.sessions.get(&id).is_some_and(|s| {
            !s.sources.is_empty()
                && s.sources
                    .iter()
                    .all(|source| self.sources[source].observation.complete)
        })
    }
    fn interval(&self, id: SessionId) -> (u64, u64) {
        let source = &self.sessions[&id].sources;
        (
            source
                .iter()
                .map(|id| self.sources[id].observation.first_collection)
                .min()
                .unwrap_or(0),
            source
                .iter()
                .map(|id| self.sources[id].observation.last_collection)
                .max()
                .unwrap_or(0),
        )
    }
    fn join(&mut self, parent: SessionId, child: SessionId, actions: &mut Vec<Action>) {
        let child = self.sessions.remove(&child).unwrap();
        for source in &child.sources {
            self.sources.get_mut(source).unwrap().session = parent;
        }
        let session = self.sessions.get_mut(&parent).unwrap();
        session.sources.extend(child.sources);
        session.upper = None;
        session.collecting = child.collecting;
        session.release_at = child.release_at;
        session.ui_deadline = if session.shown {
            None
        } else {
            child.ui_deadline
        };
        session.shown |= child.shown;
        session.generation += 1;
        if self.active == Some(child.id) {
            self.active = Some(parent);
        }
        actions.push(Action::MergeSession {
            from: child.id,
            into: parent,
        });
    }
    fn emit_gesture(&mut self, gesture: Gesture, first: u64, last: u64, actions: &mut Vec<Action>) {
        self.next_gesture += 1;
        let event = CompletedGesture {
            id: self.next_gesture,
            gesture,
            first_collection: first,
            last_collection: last,
        };
        self.last_gesture = Some(event.clone());
        actions.push(Action::Gesture(event));
    }
    fn retire(&mut self, id: SessionId, actions: &mut Vec<Action>) {
        if let Some(session) = self.sessions.remove(&id) {
            for source in &session.sources {
                self.sources.remove(source);
            }
            for child in self.sessions.values_mut() {
                if child.resume.is_some_and(|r| r.parent == id) {
                    child.resume = None;
                }
            }
            if self.active == Some(id) {
                self.active = None;
            }
            actions.push(Action::Retire {
                session: id,
                sources: session.sources,
            });
        }
    }
    fn tap(&mut self, id: SessionId, now: u64, actions: &mut Vec<Action>) {
        let (first, last) = self.interval(id);
        let session = self.sessions.get_mut(&id).unwrap();
        let grace = session.policy.tap_sequence_grace_ms;
        let prefix = session.prefix.take().or_else(|| {
            self.pending_tap
                .as_ref()
                .filter(|t| t.accepts(now, Some(first)))
                .cloned()
        });
        let already_resumed = self
            .sessions
            .values()
            .find(|s| s.resume.is_some_and(|r| r.parent == id && r.within_grace))
            .map(|s| s.id);
        self.retire(id, actions);
        if let Some(prefix) = prefix {
            self.pending_tap = None;
            self.emit_gesture(Gesture::DoublePush, prefix.first, last, actions);
        } else {
            // A different completed tap must not silently replace an older one.
            if let Some(old) = self.pending_tap.take() {
                self.emit_gesture(Gesture::SinglePush, old.first, old.last, actions);
            }
            let tap = Tap {
                first,
                last,
                deadline: now.saturating_add(grace),
                watermark: None,
                waiting_for_range: false,
            };
            if let Some(child) = already_resumed {
                // S for the next operation can arrive before the first short's
                // final C. Preserve that accepted operation as the prefix owner.
                self.sessions.get_mut(&child).unwrap().prefix = Some(tap);
            } else {
                self.pending_tap = Some(tap);
            }
        }
    }
    fn timers(&mut self, now: u64, inclusive: bool, actions: &mut Vec<Action>) {
        let due = |deadline| {
            if inclusive {
                deadline <= now
            } else {
                deadline < now
            }
        };
        for session in self.sessions.values_mut() {
            if session.ui_deadline.is_some_and(due) {
                session.ui_deadline = None;
                if session.collecting && !session.failed {
                    session.shown = true;
                }
            }
        }
        if let Some(tap) = &mut self.pending_tap
            && due(tap.deadline)
        {
            if !tap.waiting_for_range
                && tap.watermark.is_none()
                && self.known_end > self.processed_end
                && self.known_end > tap.last.saturating_add(1)
            {
                tap.watermark = Some(self.known_end);
            }
            if tap.watermark.is_none() && self.range_pending {
                tap.waiting_for_range = true;
            }
            if !tap.waiting_for_range && tap.watermark.is_none_or(|end| self.processed_end >= end) {
                let tap = self.pending_tap.take().unwrap();
                self.emit_gesture(Gesture::SinglePush, tap.first, tap.last, actions);
            }
        }
    }
    fn settle(&mut self, now: u64, actions: &mut Vec<Action>) {
        // Resolve provisional continuations before deciding whether a parent is
        // ready for batch. A rejected short never enters the parent's final PCM.
        let resumes: Vec<_> = self
            .sessions
            .values()
            .filter_map(|s| s.resume.map(|r| (s.id, r)))
            .collect();
        for (child, resume) in resumes {
            let child_class = self.classification(child);
            let parent_class = self.classification(resume.parent);
            let parent_final = self.sessions.get(&resume.parent).is_some_and(|s| {
                !s.sources.is_empty()
                    && s.sources
                        .iter()
                        .all(|id| self.sources[id].observation.final_seen)
            });
            if resume.within_grace
                && parent_final
                && child_class == Some(Press::Long)
                && parent_class == Some(Press::Long)
            {
                self.join(resume.parent, child, actions);
            } else if child_class == Some(Press::Short)
                || self.complete(child) && child_class.is_none()
                || !resume.within_grace && !self.sessions[&child].sources.is_empty()
                || self.complete(resume.parent) && parent_class.is_none()
            {
                self.sessions.get_mut(&child).unwrap().resume = None;
            }
        }
        let ids: Vec<_> = self.sessions.keys().copied().collect();
        for id in ids {
            if self.sessions[&id].submitted || self.sessions[&id].failed {
                continue;
            }
            let session = &self.sessions[&id];
            if session.sources.is_empty()
                && !session.collecting
                && session
                    .upper
                    .is_some_and(|end| end == session.first && self.processed_end >= end)
            {
                // A later candidate with the same receive boundary and a decoded
                // watermark proves this S pulse owns no collections. No gesture
                // or empty recognition result is invented for it.
                self.retire(id, actions);
                continue;
            }
            let classification = self.classification(id);
            if classification == Some(Press::Short) {
                let session = self.sessions.get_mut(&id).unwrap();
                session.shown = false;
                session.ui_deadline = None;
                if self.complete(id) {
                    self.tap(id, now, actions);
                }
                continue;
            }
            let session = &self.sessions[&id];
            let ended = !session.collecting && session.deadline().is_some_and(|d| now >= d);
            let tentative_child = self
                .sessions
                .values()
                .any(|s| s.resume.is_some_and(|r| r.parent == id));
            if ended && self.complete(id) && !tentative_child && session.resume.is_none() {
                let prefix = session.prefix.clone();
                if classification == Some(Press::Long) {
                    let (first, last) = self.interval(id);
                    self.emit_gesture(
                        if prefix.is_some() {
                            Gesture::SingleThenHold
                        } else {
                            Gesture::LongHold
                        },
                        prefix.map_or(first, |t| t.first),
                        last,
                        actions,
                    );
                } else {
                    for source in &self.sessions[&id].sources {
                        actions.push(Action::Ambiguous {
                            source: source.clone(),
                            reason: "complete audio without attributable button classification",
                        });
                    }
                    if let Some(prefix) = prefix {
                        self.emit_gesture(Gesture::SinglePush, prefix.first, prefix.last, actions);
                    }
                }
                let session = self.sessions.get_mut(&id).unwrap();
                session.submitted = true;
                session.generation += 1;
                actions.push(Action::Recognize {
                    session: id,
                    sources: session.sources.clone(),
                });
                if self.active == Some(id) {
                    self.active = None;
                }
            }
        }
    }
    fn transition(&self, actions: Vec<Action>) -> Transition {
        let mut sessions = BTreeMap::new();
        for session in self.sessions.values() {
            let short = self.classification(session.id) == Some(Press::Short);
            let live = !session.submitted
                && !session.failed
                && !short
                && (session.collecting || session.deadline().is_some_and(|d| self.now < d));
            sessions.insert(
                session.id,
                SessionView {
                    id: session.id,
                    generation: session.generation,
                    sources: session.sources.clone(),
                    visible: session.shown && !short,
                    collecting: session.collecting,
                    live,
                    dictating: !session.collecting && !live && !short,
                    failed: session.failed,
                    policy: session.policy,
                },
            );
        }
        for child in self.sessions.values() {
            if let Some(resume) = child.resume.filter(|r| r.within_grace)
                && let Some(provisional) = sessions.remove(&child.id)
                && let Some(parent) = sessions.get_mut(&resume.parent)
            {
                parent.sources.extend(provisional.sources);
                parent.collecting = provisional.collecting;
                parent.live = provisional.live;
                parent.dictating = provisional.dictating;
                parent.visible |= provisional.visible;
            }
        }
        let current = self.active.and_then(|id| self.sessions.get(&id));
        let effective_id = current.map(|s| {
            s.resume
                .filter(|r| r.within_grace)
                .map_or(s.id, |r| r.parent)
        });
        let visible_recording = sessions.values().any(|s| s.visible && s.live);
        let app_state = if visible_recording {
            AppState::Recording
        } else if sessions.values().any(|s| s.failed) {
            AppState::Error
        } else if sessions.values().any(|s| s.dictating) {
            AppState::Dictating
        } else {
            AppState::Idle
        };
        let gesture_state = if self.pending_tap.is_some() {
            GestureState::SinglePending
        } else if let Some(s) = current {
            if s.resume.is_some() {
                GestureState::ResumePending
            } else if s.collecting {
                GestureState::Holding
            } else {
                GestureState::ReleasePending
            }
        } else {
            GestureState::Idle
        };
        Transition {
            snapshot: Snapshot {
                session_id: effective_id.or_else(|| sessions.keys().next_back().copied()),
                gesture_state,
                prefix: current
                    .and_then(|s| s.prefix.as_ref())
                    .map(|_| Gesture::SinglePush),
                last_completed_gesture: self.last_gesture.clone(),
                app_state,
                collecting: current.is_some_and(|s| s.collecting),
                connected: self.connected,
                ui_deadline: current.and_then(|s| s.ui_deadline),
                resume_deadline: current.and_then(Session::deadline),
                tap_deadline: self.pending_tap.as_ref().map(|t| t.deadline),
                generation: self.generation,
            },
            sessions: sessions.into_values().collect(),
            actions,
        }
    }
}
