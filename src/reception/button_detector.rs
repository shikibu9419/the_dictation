//! Normalize record83 snapshots; do not reconstruct physical switch edges or
//! classify taps from audio duration. S, history entries, and final are separate.
use anyhow::{Result, ensure};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Press {
    Short,
    Long,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Initial,
    Extended,
    Repeated,
    Missing,
    ResetOrDivergence,
    OutOfOrder,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryDelta {
    pub collection: u64,
    pub snapshot: Option<Vec<Press>>,
    pub added: Vec<Press>,
    pub evidence: Evidence,
}
#[derive(Default)]
pub struct ButtonHistory {
    last_collection: Option<u64>,
    prefix: Option<Vec<Press>>,
}
impl ButtonHistory {
    pub fn observe(&mut self, collection: u64, snapshot: Option<&[Press]>) -> Result<HistoryDelta> {
        ensure!(
            snapshot.is_none_or(|s| s.len() <= 32),
            "Button history exceeds record83 capacity"
        );
        let mut delta = HistoryDelta {
            collection,
            snapshot: snapshot.map(<[Press]>::to_vec),
            added: vec![],
            evidence: Evidence::Missing,
        };
        if self.last_collection.is_some_and(|last| collection <= last) {
            delta.evidence = Evidence::OutOfOrder;
            return Ok(delta);
        }
        self.last_collection = Some(collection);
        let Some(snapshot) = snapshot else {
            return Ok(delta);
        };
        match &self.prefix {
            None => {
                delta.added = snapshot.to_vec();
                delta.evidence = Evidence::Initial;
            }
            Some(previous) if snapshot == previous => delta.evidence = Evidence::Repeated,
            Some(previous) if snapshot.starts_with(previous) => {
                delta.added = snapshot[previous.len()..].to_vec();
                delta.evidence = Evidence::Extended;
            }
            // No protocol evidence says a shorter/divergent snapshot is a new
            // physical press. Seed a baseline and report the ambiguity.
            Some(_) => delta.evidence = Evidence::ResetOrDivergence,
        }
        self.prefix = Some(snapshot.to_vec());
        Ok(delta)
    }
}

/// Per-source metadata classification. An initial nonfinal snapshot can contain
/// history predating this audio source. A known long source is never reclassified
/// as short just because another short was appended to its history.
#[derive(Default, Debug)]
pub struct SourceClassification(Option<Press>);
impl SourceClassification {
    pub fn observe(
        &mut self,
        delta: &HistoryDelta,
        first_collection: bool,
        final_part: bool,
    ) -> Option<Press> {
        if self.0 == Some(Press::Long) {
            return self.0;
        }
        // A fresh, complete one-collection source owns its singleton metadata.
        // Record83 is not a device-wide monotonic counter: separate short
        // sources can each carry [Short], including immediately after [Long].
        // Source/replay identity is checked by the receive store before here.
        if first_collection
            && final_part
            && delta.evidence != Evidence::OutOfOrder
            && let Some([press]) = delta.snapshot.as_deref()
        {
            self.0 = Some(*press);
            return self.0;
        }
        let trustworthy = delta.evidence == Evidence::Extended
            || (delta.evidence == Evidence::Initial && first_collection && final_part);
        if trustworthy && delta.added.len() == 1 {
            self.0 = Some(delta.added[0]);
        }
        self.0
    }
    pub fn value(&self) -> Option<Press> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Press::*;
    #[test]
    fn cumulative_history_only_emits_new_suffix_and_ignores_replays() {
        let mut history = ButtonHistory::default();
        let mut source = SourceClassification::default();
        let first = history.observe(2579, Some(&[Short])).unwrap();
        assert_eq!(source.observe(&first, true, false), None);
        assert!(
            history
                .observe(2580, Some(&[Short]))
                .unwrap()
                .added
                .is_empty()
        );
        let next = history.observe(2581, Some(&[Short, Long])).unwrap();
        assert_eq!(next.added, [Long]);
        assert_eq!(source.observe(&next, false, false), Some(Long));
        let end = history.observe(2582, Some(&[Short, Long, Short])).unwrap();
        assert_eq!(end.added, [Short]);
        assert_eq!(source.observe(&end, false, true), Some(Long));
        assert!(
            history
                .observe(2582, Some(&[Short, Long, Short]))
                .unwrap()
                .added
                .is_empty()
        );
    }
    #[test]
    fn stale_collection_cannot_roll_back_the_prefix() {
        let mut history = ButtonHistory::default();
        history.observe(10, Some(&[Short])).unwrap();
        assert_eq!(
            history.observe(9, Some(&[])).unwrap().evidence,
            Evidence::OutOfOrder
        );
        assert_eq!(
            history.observe(11, Some(&[Short, Long])).unwrap().added,
            [Long]
        );
    }
    #[test]
    fn reset_divergence_and_absent_metadata_do_not_invent_gestures() {
        let mut history = ButtonHistory::default();
        history.observe(1, Some(&[Short, Long])).unwrap();
        assert_eq!(
            history.observe(2, None).unwrap().evidence,
            Evidence::Missing
        );
        assert_eq!(
            history.observe(3, Some(&[Short, Long])).unwrap().evidence,
            Evidence::Repeated
        );
        let shorter = history.observe(4, Some(&[Long])).unwrap();
        assert!(shorter.added.is_empty());
        assert_eq!(shorter.evidence, Evidence::ResetOrDivergence);
        assert!(
            history
                .observe(5, Some(&[Short, Long]))
                .unwrap()
                .added
                .is_empty()
        );
        assert_eq!(
            history
                .observe(6, Some(&[Short, Long, Short]))
                .unwrap()
                .added,
            [Short]
        );
    }
    #[test]
    fn final_single_entry_can_classify_without_a_state_edge_but_ambiguous_history_cannot() {
        for press in [Short, Long] {
            let mut history = ButtonHistory::default();
            let first = history.observe(1, Some(&[press])).unwrap();
            assert_eq!(
                SourceClassification::default().observe(&first, true, true),
                Some(press)
            );
        }
        let mut history = ButtonHistory::default();
        let first = history.observe(1, Some(&[Short, Long])).unwrap();
        assert_eq!(
            SourceClassification::default().observe(&first, true, true),
            None
        );
    }
    #[test]
    fn separate_completed_singletons_survive_history_reset_and_repetition() {
        let mut history = ButtonHistory::default();
        history.observe(2804, Some(&[Long])).unwrap();
        for index in 2805..=2807 {
            let delta = history.observe(index, Some(&[Short])).unwrap();
            assert_eq!(
                SourceClassification::default().observe(&delta, true, true),
                Some(Short)
            );
            assert_eq!(
                SourceClassification::default().observe(&delta, true, false),
                None
            );
            assert_eq!(
                SourceClassification::default().observe(&delta, false, true),
                None
            );
        }
        let next = history.observe(2808, Some(&[Short, Short])).unwrap();
        assert_eq!(
            SourceClassification::default().observe(&next, true, true),
            Some(Short)
        );
        let replay = history.observe(2808, Some(&[Short])).unwrap();
        assert_eq!(
            SourceClassification::default().observe(&replay, true, true),
            None
        );
    }
    #[test]
    fn invalid_snapshot_is_rejected_without_advancing_the_observation() {
        let mut history = ButtonHistory::default();
        assert!(history.observe(10, Some(&[Short; 33])).is_err());
        assert_eq!(
            history.observe(10, Some(&[Long])).unwrap().evidence,
            Evidence::Initial
        );
    }
}
