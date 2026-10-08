//! Convert reducer decisions into bounded PCM cursor updates. The source store
//! owns audio; this executor retains only IDs, cursors and presentation state.
use super::session_state::{Action, SessionId, SessionView, Snapshot, SourceId, Transition};
use crate::pcm::Pcm;
use anyhow::{Result, ensure};
use std::{collections::BTreeMap, ops::Range};

#[derive(Clone, Copy, Debug)]
pub struct SourceProgress {
    /// Contiguous, decoded samples. PCM beyond a missing C is not yet readable.
    pub samples: usize,
    /// Metadata-only prefixes do not establish a sample rate.
    pub rate: Option<u32>,
    pub complete: bool,
}
pub trait AudioStore {
    fn progress(&self, source: &str) -> Result<SourceProgress>;
    fn slice(&self, source: &str, samples: Range<usize>) -> Result<Pcm>;
}
#[derive(Debug)]
pub struct AudioPlan {
    pub session: SessionId,
    pub generation: u64,
    pub start_sample: usize,
    pub rate: u32,
    pub pcm: Pcm,
}
#[derive(Debug)]
pub enum Effect {
    Snapshot(Snapshot),
    View(SessionView),
    ResetLive {
        session: SessionId,
        generation: u64,
    },
    Live(AudioPlan),
    StopLive(SessionId),
    Batch(AudioPlan),
    /// Includes gesture hooks, store releases and diagnostic reasons.
    Action(Action),
}
#[derive(Clone, Default)]
struct Cursor {
    sources: Vec<SourceId>,
    sent: usize,
    generation: u64,
    live: bool,
    view: Option<SessionView>,
}
#[derive(Clone, Default)]
pub struct InputEffects {
    cursors: BTreeMap<SessionId, Cursor>,
}
struct Span {
    id: SourceId,
    start: usize,
    end: usize,
}
struct AudioView {
    spans: Vec<Span>,
    samples: usize,
    rate: u32,
}
impl AudioView {
    fn read(sources: &[SourceId], store: &impl AudioStore, whole: bool) -> Result<Self> {
        let mut view = Self {
            spans: vec![],
            samples: 0,
            rate: 0,
        };
        for id in sources {
            let source = store.progress(id)?;
            ensure!(
                !whole || source.complete,
                "Cannot recognize incomplete source={id}"
            );
            if source.samples > 0 {
                let rate = source
                    .rate
                    .filter(|r| *r > 0)
                    .ok_or_else(|| anyhow::anyhow!("Audio has no sample rate source={id}"))?;
                ensure!(
                    view.rate == 0 || view.rate == rate,
                    "Sample rate changed between joined sources"
                );
                view.rate = rate;
            }
            view.spans.push(Span {
                id: id.clone(),
                start: view.samples,
                end: view.samples + source.samples,
            });
            view.samples += source.samples;
            // A later source must not overtake a missing tail in the first one.
            // Volume metering uses the receive store directly and is unaffected.
            if !source.complete {
                break;
            }
        }
        if view.rate == 0 {
            view.rate = 16000;
        }
        Ok(view)
    }
    fn pcm(&self, store: &impl AudioStore, range: Range<usize>) -> Result<Pcm> {
        ensure!(
            range.start <= range.end && range.end <= self.samples,
            "PCM cursor outside retained source range"
        );
        let mut pcm = Pcm::default();
        for span in &self.spans {
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);
            if start < end {
                let block = store.slice(&span.id, start - span.start..end - span.start)?;
                ensure!(
                    block.len() == end - start,
                    "Source store returned an incomplete PCM slice"
                );
                pcm.append(&block);
            }
        }
        Ok(pcm)
    }
}
impl InputEffects {
    /// No ASR wait occurs here. A caller forwards the plans to its recognition
    /// scheduler; a failed store read leaves every cursor unchanged for retry.
    pub fn reconcile(
        &mut self,
        transition: Transition,
        store: &impl AudioStore,
        live_enabled: bool,
    ) -> Result<Vec<Effect>> {
        let mut next = self.clone();
        let effects = next.apply(transition, store, live_enabled)?;
        *self = next;
        Ok(effects)
    }
    fn apply(
        &mut self,
        transition: Transition,
        store: &impl AudioStore,
        live_enabled: bool,
    ) -> Result<Vec<Effect>> {
        let mut effects = vec![Effect::Snapshot(transition.snapshot)];
        let removed: Vec<_> = self
            .cursors
            .keys()
            .filter(|id| !transition.sessions.iter().any(|v| v.id == **id))
            .copied()
            .collect();
        for id in removed {
            let cursor = self.cursors.remove(&id).unwrap();
            if cursor.live || cursor.sent > 0 {
                effects.push(Effect::StopLive(id));
            }
        }
        for view in transition.sessions {
            let audio = AudioView::read(&view.sources, store, false)?;
            let cursor = self.cursors.entry(view.id).or_default();
            // A provisional source may later be identified as short. Only PCM
            // actually sent beyond the retained common prefix needs a reset.
            let common = cursor
                .sources
                .iter()
                .zip(&view.sources)
                .take_while(|(a, b)| a == b)
                .count();
            let retained_prefix = audio
                .spans
                .iter()
                .take(common)
                .next_back()
                .map_or(0, |s| s.end);
            if cursor.sent > retained_prefix {
                cursor.generation += 1;
                cursor.sent = 0;
                effects.push(Effect::ResetLive {
                    session: view.id,
                    generation: cursor.generation,
                });
            }
            cursor.sources = view.sources.clone();
            let enabled = live_enabled && view.live && !view.failed;
            if cursor.live && !enabled {
                effects.push(Effect::StopLive(view.id));
            }
            // StopLive cancels the model context; if an unfinished source resumes
            // after grace, rebuild from retained PCM instead of omitting its head.
            if !cursor.live && enabled && cursor.sent > 0 {
                cursor.generation += 1;
                cursor.sent = 0;
                effects.push(Effect::ResetLive {
                    session: view.id,
                    generation: cursor.generation,
                });
            }
            cursor.live = enabled;
            if cursor.view.as_ref() != Some(&view) {
                effects.push(Effect::View(view.clone()));
            }
            cursor.view = Some(view.clone());
            let minimum =
                (u64::from(audio.rate) * view.policy.live_chunk_ms).div_ceil(1000) as usize;
            if enabled && audio.samples.saturating_sub(cursor.sent) >= minimum {
                let pcm = audio.pcm(store, cursor.sent..audio.samples)?;
                effects.push(Effect::Live(AudioPlan {
                    session: view.id,
                    generation: cursor.generation,
                    start_sample: cursor.sent,
                    rate: audio.rate,
                    pcm,
                }));
                cursor.sent = audio.samples;
            }
        }
        for action in transition.actions {
            if let Action::Recognize { session, sources } = &action {
                let audio = AudioView::read(sources, store, true)?;
                let cursor = self.cursors.get(session);
                effects.push(Effect::Batch(AudioPlan {
                    session: *session,
                    generation: cursor.map_or(0, |c| c.generation),
                    start_sample: 0,
                    rate: audio.rate,
                    pcm: audio.pcm(store, 0..audio.samples)?,
                }));
            }
            effects.push(Effect::Action(action));
        }
        Ok(effects)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reception::{
        button_detector::Press,
        session_state::{Observation, SessionState, SourceObservation},
    };
    use std::collections::HashMap;
    #[derive(Default)]
    struct Store(HashMap<String, (Pcm, bool)>);
    impl Store {
        fn set(&mut self, id: &str, samples: Vec<i16>, complete: bool) {
            self.0.insert(id.into(), (samples.into(), complete));
        }
    }
    impl AudioStore for Store {
        fn progress(&self, source: &str) -> Result<SourceProgress> {
            let (pcm, complete) = self
                .0
                .get(source)
                .ok_or_else(|| anyhow::anyhow!("missing source"))?;
            Ok(SourceProgress {
                samples: pcm.len(),
                rate: Some(1000),
                complete: *complete,
            })
        }
        fn slice(&self, source: &str, samples: Range<usize>) -> Result<Pcm> {
            Ok(self.0[source].0.range(samples))
        }
    }
    fn source(
        m: &mut SessionState,
        now: u64,
        id: &str,
        index: u64,
        press: Option<Press>,
        complete: bool,
    ) -> Transition {
        m.observe(
            now,
            Observation::Source(SourceObservation {
                id: id.into(),
                first_collection: index,
                last_collection: index,
                classification: press,
                final_seen: complete,
                complete,
            }),
        )
        .unwrap()
    }
    fn plans(effects: &[Effect], batch: bool) -> Vec<&AudioPlan> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Live(p) if !batch => Some(p),
                Effect::Batch(p) if batch => Some(p),
                _ => None,
            })
            .collect()
    }
    #[test]
    fn live_only_gets_new_pcm_and_full_batch_includes_small_tail() {
        let mut m = SessionState::default();
        let mut effects = InputEffects::default();
        let mut store = Store::default();
        m.observe(
            0,
            Observation::Collecting {
                active: true,
                unread: 1,
            },
        )
        .unwrap();
        store.set("a", vec![1; 199], false);
        let e = effects
            .reconcile(
                source(&mut m, 10, "a", 1, Some(Press::Long), false),
                &store,
                true,
            )
            .unwrap();
        assert!(plans(&e, false).is_empty());
        store.set("a", vec![1; 400], false);
        let e = effects
            .reconcile(m.tick(50).unwrap(), &store, true)
            .unwrap();
        assert_eq!(plans(&e, false)[0].pcm.len(), 400);
        assert_eq!(plans(&e, false)[0].start_sample, 0);
        assert!(
            plans(
                &effects
                    .reconcile(m.tick(60).unwrap(), &store, true)
                    .unwrap(),
                false
            )
            .is_empty()
        );
        store.set("a", vec![1; 601], false);
        let e = effects
            .reconcile(m.tick(70).unwrap(), &store, true)
            .unwrap();
        assert_eq!(plans(&e, false)[0].start_sample, 400);
        assert_eq!(plans(&e, false)[0].pcm.len(), 201);
        store.set("a", vec![1; 650], true);
        effects
            .reconcile(
                source(&mut m, 100, "a", 1, Some(Press::Long), true),
                &store,
                true,
            )
            .unwrap();
        let e = effects
            .reconcile(m.tick(150).unwrap(), &store, true)
            .unwrap();
        assert_eq!(plans(&e, true)[0].pcm.len(), 650);
        assert!(e.iter().any(|e| matches!(e, Effect::StopLive(_))));
    }
    #[test]
    fn disabled_live_keeps_full_audio_for_batch() {
        let mut m = SessionState::default();
        let mut effects = InputEffects::default();
        let mut store = Store::default();
        store.set("a", vec![42; 7], true);
        let e = effects
            .reconcile(
                source(&mut m, 0, "a", 1, Some(Press::Long), true),
                &store,
                false,
            )
            .unwrap();
        assert!(plans(&e, false).is_empty());
        let e = effects
            .reconcile(m.tick(50).unwrap(), &store, false)
            .unwrap();
        assert_eq!(plans(&e, true)[0].pcm.len(), 7); // No 150ms heuristic here.
        assert!(plans(&e, false).is_empty());
    }
    #[test]
    fn short_after_provisional_live_rebuilds_only_retained_long_pcm() {
        let mut m = SessionState::default();
        let mut effects = InputEffects::default();
        let mut store = Store::default();
        m.observe(
            0,
            Observation::Collecting {
                active: true,
                unread: 1,
            },
        )
        .unwrap();
        store.set("a", vec![1; 250], true);
        effects
            .reconcile(
                source(&mut m, 50, "a", 1, Some(Press::Long), true),
                &store,
                true,
            )
            .unwrap();
        m.observe(
            70,
            Observation::Collecting {
                active: true,
                unread: 2,
            },
        )
        .unwrap();
        store.set("b", vec![9; 250], false);
        let e = effects
            .reconcile(source(&mut m, 80, "b", 2, None, false), &store, true)
            .unwrap();
        assert_eq!(
            plans(&e, false)[0].pcm.iter().copied().collect::<Vec<_>>(),
            vec![9; 250]
        );
        store.set("b", vec![9; 250], true);
        let e = effects
            .reconcile(
                source(&mut m, 90, "b", 2, Some(Press::Short), true),
                &store,
                true,
            )
            .unwrap();
        assert!(
            e.iter()
                .any(|e| matches!(e, Effect::ResetLive { generation: 1, .. }))
        );
        assert_eq!(
            plans(&e, false)[0].pcm.iter().copied().collect::<Vec<_>>(),
            vec![1; 250]
        );
        let e = effects
            .reconcile(m.tick(100).unwrap(), &store, true)
            .unwrap();
        assert_eq!(
            plans(&e, true)[0].pcm.iter().copied().collect::<Vec<_>>(),
            vec![1; 250]
        );
    }
    #[test]
    fn later_source_cannot_overtake_an_incomplete_tail() {
        let mut store = Store::default();
        store.set("a", vec![1; 200], false);
        store.set("b", vec![2; 200], true);
        let ids = vec!["a".into(), "b".into()];
        let view = AudioView::read(&ids, &store, false).unwrap();
        assert_eq!(view.pcm(&store, 0..view.samples).unwrap().len(), 200);
        assert!(AudioView::read(&ids, &store, true).is_err());
        store.set("a", vec![1; 250], true);
        let view = AudioView::read(&ids, &store, true).unwrap();
        assert_eq!(view.samples, 450);
        assert_eq!(
            view.pcm(&store, 200..300)
                .unwrap()
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [vec![1; 50], vec![2; 50]].concat()
        );
    }
    #[test]
    fn failed_store_read_does_not_consume_the_live_cursor() {
        let mut m = SessionState::default();
        let mut effects = InputEffects::default();
        let mut store = Store::default();
        m.observe(
            0,
            Observation::Collecting {
                active: true,
                unread: 1,
            },
        )
        .unwrap();
        let transition = source(&mut m, 50, "a", 1, Some(Press::Long), false);
        assert!(effects.reconcile(transition.clone(), &store, true).is_err());
        store.set("a", vec![1; 200], false);
        let e = effects.reconcile(transition, &store, true).unwrap();
        assert_eq!(plans(&e, false)[0].start_sample, 0);
    }
}
