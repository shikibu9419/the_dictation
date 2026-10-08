//! Receive-owned audio sources. UI/gesture sessions reference these sources; they
//! must never rename a source, rewrite its final marker, or infer final from time.
use crate::{
    collection::{Collection, decode},
    output::Output,
    pcm::Pcm,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

#[derive(Clone)]
pub struct Part {
    pub key: String,
    pub samples: Pcm,
    pub rate: u32,
    pub final_part: bool,
    pub next: u16,
    pub index: u16,
    pub buttons: Option<Vec<String>>,
    pub lifetime_count: Option<u32>,
}

#[derive(Default)]
pub struct Received {
    pub parts: Vec<Part>,
    /// Metadata must reach the reducer even when the contiguous PCM is empty.
    pub source: Option<String>,
    pub lost: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    epoch: String,
    pub position: u64,
}

pub struct RangeUpdate {
    pub start: u64,
    pub end: u64,
    pub discontinuity: bool,
    pub lost: Vec<String>,
}

#[derive(Clone, Copy)]
struct Limits {
    samples: usize,
    collections: usize,
    pending: usize,
    sources: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            samples: 64 * 1024 * 1024,
            collections: 16384,
            pending: 2048,
            sources: 128,
        }
    }
}
struct StoredPart {
    digest: [u8; 32],
    part: Part,
}
struct Source {
    epoch: uuid::Uuid,
    first: u64,
    next: u64,
    rate: Option<u32>,
    final_at: Option<u64>,
    /// Includes delivered blocks: consumers only own shared views, not copies.
    parts: BTreeMap<u64, StoredPart>,
    /// Indexed shared view of the gapless prefix; used by live PCM cursors.
    pcm: Pcm,
    samples: usize,
    lost: bool,
}
impl Source {
    fn complete(&self) -> bool {
        !self.lost && self.final_at.is_some_and(|end| self.next > end)
    }
    fn pending(&self) -> usize {
        self.parts.range(self.next..).count()
    }
}

pub struct Recordings {
    device: String,
    epoch: uuid::Uuid,
    sources: HashMap<String, Source>,
    /// Last observed wire index extended across u16 wrap. Unordered arrivals
    /// within half a wire cycle resolve to the same absolute position.
    high_water: Option<u64>,
    boundary: Option<u64>,
    available_start: Option<u64>,
    last_range: Option<(u64, u64)>,
    released: HashSet<String>,
    released_order: VecDeque<String>,
    samples: usize,
    collections: usize,
    pending: usize,
    limits: Limits,
}
impl Default for Recordings {
    fn default() -> Self {
        Self::new("unknown-device")
    }
}
impl Recordings {
    pub fn new(device: &str) -> Self {
        Self {
            device: device.into(),
            epoch: uuid::Uuid::new_v4(),
            sources: HashMap::new(),
            high_water: None,
            boundary: None,
            available_start: None,
            last_range: None,
            released: HashSet::new(),
            released_order: VecDeque::new(),
            samples: 0,
            collections: 0,
            pending: 0,
            limits: Limits::default(),
        }
    }
    pub fn position(&self, index: u16) -> u64 {
        let high = self.high_water.unwrap_or(65536 + u64::from(index));
        high.checked_add_signed(i64::from(index.wrapping_sub(high as u16) as i16))
            .expect("wire position has a full-cycle origin")
    }
    pub fn observation(
        &self,
        key: &str,
        classification: Option<pebble_index::reception::button_detector::Press>,
    ) -> Result<pebble_index::reception::session_state::SourceObservation> {
        let source = self.sources.get(key).context("Unknown audio source")?;
        Ok(pebble_index::reception::session_state::SourceObservation {
            id: key.into(),
            first_collection: source.first,
            last_collection: *source.parts.keys().next_back().context("Empty source")?,
            classification,
            final_seen: source.final_at.is_some(),
            complete: source.complete(),
        })
    }
    pub fn checkpoint(&self, key: &str) -> Result<Checkpoint> {
        let source = self.sources.get(key).context("Unknown audio source")?;
        ensure!(
            source.complete(),
            "Cannot checkpoint incomplete source={key}"
        );
        Ok(Checkpoint {
            epoch: source.epoch.to_string(),
            position: source.next,
        })
    }
    pub fn current_checkpoint(&self, checkpoint: &Checkpoint) -> bool {
        checkpoint.epoch == self.epoch.to_string()
    }
    /// Startup boundary only. Reconnecting must keep both sources and cursors.
    pub fn reset(&mut self, index: u16) -> Result<()> {
        ensure!(
            self.sources.is_empty(),
            "Cannot reset a receive store with retained audio"
        );
        let position = 65536 + u64::from(index);
        self.epoch = uuid::Uuid::new_v4();
        self.high_water = Some(position);
        self.boundary = Some(position);
        self.available_start = None;
        self.last_range = None;
        self.released.clear();
        self.released_order.clear();
        Ok(())
    }
    pub fn add(&mut self, index: u16, raw: &[u8], output: &Output) -> Result<Received> {
        ensure!(raw.len() <= 655360, "Collection exceeds receive size limit");
        let position = self.position(index);
        if self.available_start.is_some_and(|start| position < start) {
            output.debug(format!(
                "Skipping expired collection={index}; position={position}"
            ));
            return Ok(Received::default());
        }
        let item = decode(raw)?; // One TLV pass; diagnostics and gestures reuse its headers.
        let (first, origin) = if item.multipart {
            let start = item
                .start
                .context("Multipart recording lacks starting collection count")?;
            let distance = u64::from(index.wrapping_sub(start as u16));
            ensure!(distance < 32768, "Ambiguous multipart collection offset");
            (
                position
                    .checked_sub(distance)
                    .context("Invalid source start")?,
                format!("multipart-{start}"),
            )
        } else {
            (position, format!("collection-{position}"))
        };
        let key = format!("{}/{}/{}", self.device, self.epoch, origin);
        if self.boundary.is_some_and(|boundary| first < boundary) {
            output.debug(format!(
                "Skipping pre-start source={key} collection={index}"
            ));
            return Ok(Received::default());
        }
        if self.released.contains(&key) {
            output.debug(format!(
                "Ignoring replay of retired source={key} collection={index}"
            ));
            return Ok(Received::default());
        }
        let digest: [u8; 32] = Sha256::digest(raw).into();
        let final_part = !item.multipart || item.final_part;
        let count = item.samples.as_ref().map_or(0, Vec::len);
        let is_pending = self.sources.get(&key).map_or(first, |s| s.next) != position;
        if let Some(source) = self.sources.get(&key) {
            ensure!(
                source.first == first,
                "Source identity reused with a different start: {key}"
            );
            if let Some(existing) = source.parts.get(&position) {
                ensure!(
                    existing.digest == digest,
                    "Conflicting retransmission source={key} collection={index}"
                );
                output.debug(format!(
                    "Ignoring duplicate source={key} collection={index}"
                ));
                return Ok(Received::default());
            }
            ensure!(
                source.final_at.is_none_or(|end| position <= end),
                "Audio after final source={key}"
            );
            ensure!(
                !final_part || source.final_at.is_none_or(|end| end == position),
                "Conflicting final source={key}"
            );
            ensure!(
                !final_part || source.parts.range(position + 1..).next().is_none(),
                "Final precedes retained audio source={key}"
            );
            ensure!(
                source.rate.is_none() || item.rate.is_none() || source.rate == item.rate,
                "Sample rate changed within source={key}"
            );
        } else {
            ensure!(
                self.sources.len() < self.limits.sources,
                "Receive store source limit reached"
            );
        }
        // Validate capacity before mutating anything. A failed insertion is retryable.
        ensure!(
            count <= self.limits.samples.saturating_sub(self.samples),
            "Receive PCM limit reached; retained samples={}",
            self.samples
        );
        ensure!(
            self.collections < self.limits.collections,
            "Receive collection limit reached"
        );
        ensure!(
            !is_pending || self.pending < self.limits.pending,
            "Receive out-of-order limit reached"
        );
        log_collection(index, &key, raw.len(), &item, output);
        let source = self.sources.entry(key.clone()).or_insert_with(|| Source {
            epoch: self.epoch,
            first,
            next: first,
            rate: None,
            final_at: None,
            parts: BTreeMap::new(),
            pcm: Pcm::default(),
            samples: 0,
            lost: false,
        });
        let old_pending = source.pending();
        source.rate = source.rate.or(item.rate);
        if final_part {
            source.final_at = Some(position);
        }
        let part = Part {
            key: key.clone(),
            samples: item.samples.unwrap_or_default().into(),
            rate: source.rate.unwrap_or(16000),
            final_part,
            next: index.wrapping_add(1),
            index,
            buttons: item.buttons,
            lifetime_count: item.lifetime_count,
        };
        source.parts.insert(position, StoredPart { digest, part });
        source.samples += count;
        self.samples += count;
        self.collections += 1;
        self.high_water = Some(self.high_water.map_or(position, |high| high.max(position)));
        if !source.lost
            && self
                .available_start
                .is_some_and(|start| source.next < start)
        {
            source.lost = true;
            output.error(format!("Source {key} starts before the available ring range; missing collection={}; retaining {} samples without recognizing incomplete audio", source.next as u16, source.samples));
        }
        let mut ready = vec![];
        while !source.lost
            && let Some(stored) = source.parts.get(&source.next)
        {
            let mut part = stored.part.clone();
            // Metadata-only prefixes do not establish a sample rate.
            part.rate = source.rate.unwrap_or(16000);
            source.pcm.append(&part.samples);
            source.next += 1;
            ready.push(part);
        }
        self.pending = self.pending - old_pending + source.pending();
        output.debug(format!("PCM store source={key} retained_samples={} source_samples={} collections={} pending={} complete={}", self.samples, source.samples, self.collections, self.pending, source.complete()));
        Ok(Received {
            parts: ready,
            source: Some(key),
            lost: source.lost,
        })
    }
    /// Return shared blocks only when every collection through final is present.
    #[cfg(test)]
    fn whole(&self, key: &str) -> Result<Pcm> {
        let source = self.sources.get(key).context("Unknown audio source")?;
        ensure!(source.complete(), "Source {key} is not complete");
        Ok(source.pcm.clone())
    }
    /// Explicit consumer acknowledgement (recognized, cancelled, or rejected tap).
    /// Completed IDs stay in a bounded replay ledger after their PCM is released.
    pub fn release(&mut self, key: &str) {
        if let Some(source) = self.sources.remove(key) {
            self.samples -= source.samples;
            self.collections -= source.parts.len();
            self.pending -= source.pending();
            if self.released.insert(key.to_owned()) {
                self.released_order.push_back(key.to_owned());
            }
            while self.released_order.len() > 2048 {
                if let Some(old) = self.released_order.pop_front() {
                    self.released.remove(&old);
                }
            }
        }
    }
    /// A ring eviction is an error, not successful completion. Already retained
    /// PCM remains available until the session owner explicitly releases it.
    pub fn retain(&mut self, start: u16, end: u16, output: &Output) -> Result<RangeUpdate> {
        let count = u64::from(end.wrapping_sub(start));
        ensure!(count <= 512, "Invalid collection range");
        let mut end_position = self.position(end);
        let mut start_position = end_position - count;
        // A normal u16 wrap advances by a small positive delta. A regression
        // cannot share source IDs, gesture prefixes or save cursors with the
        // previous range. This detects discontinuity, not its physical cause.
        let discontinuity = self.last_range.map_or_else(
            || self.high_water.is_some_and(|high| end_position < high),
            |(old_start, old_end)| {
                pebble_index::reception::scheduler::range_regressed(
                    (old_start as u16, old_end as u16),
                    (start, end),
                )
            },
        );
        if discontinuity {
            let previous = self.epoch;
            self.epoch = uuid::Uuid::new_v4();
            end_position = (self.high_water.unwrap_or(0) / 65536 + 2) * 65536 + u64::from(end);
            start_position = end_position - count;
            // This is not a startup flush. A new source whose prefix is already
            // missing must be reported as lost, not silently skipped.
            self.boundary = None;
            output.error(format!("Ring counter discontinuity: previous_range={:?} range={start}..{end} epoch={previous}->{}; separating audio and checkpoints", self.last_range, self.epoch));
        }
        self.high_water = Some(
            self.high_water
                .map_or(end_position, |old| old.max(end_position)),
        );
        self.last_range = Some((start_position, end_position));
        self.available_start = Some(start_position);
        let mut lost = vec![];
        for (key, source) in &mut self.sources {
            if !source.complete()
                && !source.lost
                && (source.epoch != self.epoch || source.next < start_position)
            {
                source.lost = true;
                output.error(format!("Source {key} is incomplete: collection {} is no longer available in ring range {start}..{end}; retaining {} samples", source.next as u16, source.samples));
                lost.push(key.clone());
            }
        }
        Ok(RangeUpdate {
            start: start_position,
            end: end_position,
            discontinuity,
            lost,
        })
    }
}
impl pebble_index::reception::input_effects::AudioStore for Recordings {
    fn progress(
        &self,
        key: &str,
    ) -> Result<pebble_index::reception::input_effects::SourceProgress> {
        let source = self.sources.get(key).context("Unknown audio source")?;
        Ok(pebble_index::reception::input_effects::SourceProgress {
            samples: source.pcm.len(),
            rate: source.rate,
            complete: source.complete(),
        })
    }
    fn slice(&self, key: &str, range: std::ops::Range<usize>) -> Result<Pcm> {
        let source = self.sources.get(key).context("Unknown audio source")?;
        ensure!(
            range.start <= range.end && range.end <= source.pcm.len(),
            "PCM cursor exceeds contiguous receive prefix"
        );
        Ok(source.pcm.range(range))
    }
}
fn log_collection(index: u16, key: &str, bytes: usize, item: &Collection, output: &Output) {
    let header = |id| {
        item.headers.get(&id).map(|data| {
            data.iter()
                .take(32)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        })
    };
    output.debug(format!("Collection boundary evidence index={index} source={key}: metadata82={:?} button_sequence83={:?} lifetime_count84={:?}; button sequence is stored metadata, not a physical key edge", header(82), header(83), header(84)));
    output.debug(format!(
        "collection={index} bytes={bytes} multipart={} final={} recording_start={:?}",
        item.multipart, item.final_part, item.start
    ));
    output.debug(format!(
        "audio collection={index} samples={} rate={:?} buttons={:?}",
        item.samples.as_ref().map_or(0, Vec::len),
        item.rate,
        item.buttons
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output() -> Output {
        Output::new(false, None).unwrap()
    }
    #[test]
    fn range_wrap_preserves_epoch_and_initial_fetch_orders_wrapped_collections() {
        let out = output();
        let mut store = Recordings::new("wrapped-ring");
        let before = store.epoch;
        let range = store.retain(65535, 1, &out).unwrap();
        assert!(!range.discontinuity);
        assert_eq!(range.end - range.start, 2);
        let first = store.add(65535, &chunk(65535, false, &[1]), &out).unwrap();
        let key = first.source.unwrap();
        store.add(0, &chunk(65535, true, &[2]), &out).unwrap();
        assert_eq!(samples(&store.whole(&key).unwrap()), [1, 2]);
        assert!(!store.retain(0, 2, &out).unwrap().discontinuity);
        assert_eq!(store.epoch, before);
        assert!(store.current_checkpoint(&store.checkpoint(&key).unwrap()));
    }

    #[test]
    fn regressed_range_keeps_old_whole_audio_and_isolates_reused_source_ids_and_checkpoints() {
        let out = output();
        let mut store = Recordings::new("reset-ring");
        store.reset(1).unwrap();
        store.retain(1, 4, &out).unwrap();
        let old = store
            .add(1, &chunk(1, true, &[1, 2]), &out)
            .unwrap()
            .source
            .unwrap();
        let pending = store
            .add(2, &chunk(2, false, &[3]), &out)
            .unwrap()
            .source
            .unwrap();
        let checkpoint = store.checkpoint(&old).unwrap();
        let range = store.retain(1, 2, &out).unwrap();
        assert!(range.discontinuity);
        assert_eq!(range.lost, std::slice::from_ref(&pending));
        assert!(range.start > checkpoint.position);
        assert!(!store.current_checkpoint(&checkpoint));
        assert_eq!(samples(&store.whole(&old).unwrap()), [1, 2]);
        assert!(store.whole(&pending).is_err());
        assert_eq!(store.sources[&pending].samples, 1);
        let new = store
            .add(1, &chunk(1, true, &[9]), &out)
            .unwrap()
            .source
            .unwrap();
        assert_ne!(old, new);
        assert_eq!(samples(&store.whole(&new).unwrap()), [9]);
        assert!(store.current_checkpoint(&store.checkpoint(&new).unwrap()));
        store.release(&old);
        assert_eq!(samples(&store.whole(&new).unwrap()), [9]);
    }

    #[test]
    fn lost_source_retains_late_tail_without_blocking_the_next_recording() {
        let out = output();
        let mut store = Recordings::new("evicted-ring");
        store.reset(1).unwrap();
        store.retain(1, 3, &out).unwrap();
        let key = store
            .add(1, &chunk(1, false, &[1]), &out)
            .unwrap()
            .source
            .unwrap();
        assert_eq!(
            store.retain(3, 5, &out).unwrap().lost,
            std::slice::from_ref(&key)
        );
        let tail = store.add(3, &chunk(1, true, &[3]), &out).unwrap();
        assert_eq!(tail.source.as_deref(), Some(key.as_str()));
        assert!(tail.lost);
        assert!(tail.parts.is_empty());
        assert_eq!(store.sources[&key].samples, 2);
        assert!(store.whole(&key).is_err());
        let next = store
            .add(4, &chunk(4, true, &[4]), &out)
            .unwrap()
            .source
            .unwrap();
        assert_eq!(samples(&store.whole(&next).unwrap()), [4]);
        assert!(
            store
                .add(3, &chunk(1, true, &[3]), &out)
                .unwrap()
                .source
                .is_none()
        );
    }

    #[test]
    fn a_source_first_seen_after_its_prefix_was_evicted_is_owned_and_failed() {
        let out = output();
        let mut store = Recordings::new("missing-prefix-ring");
        store.reset(1).unwrap();
        store.retain(3, 4, &out).unwrap();
        let tail = store.add(3, &chunk(1, true, &[3]), &out).unwrap();
        assert!(tail.lost);
        assert!(tail.parts.is_empty());
        let key = tail.source.unwrap();
        assert!(!store.observation(&key, None).unwrap().complete);
        assert_eq!(store.sources[&key].samples, 1);
        assert!(store.checkpoint(&key).is_err());
    }
    fn wire(
        start: u32,
        multipart: bool,
        final_part: bool,
        pcm: Option<&[i16]>,
        rate: u32,
        buttons: Option<(u32, u32)>,
    ) -> Vec<u8> {
        let mut records = vec![];
        if let Some(samples) = pcm {
            let mut body = rate.to_le_bytes().to_vec();
            body.extend(samples.iter().flat_map(|v| v.to_le_bytes()));
            records.push(80);
            records.extend((body.len() as u32).to_le_bytes());
            records.extend(body);
        }
        records.push(82);
        records.extend(6u16.to_le_bytes());
        records.extend(start.to_le_bytes());
        records.extend([multipart as u8, final_part as u8]);
        if let Some((pattern, count)) = buttons {
            records.push(83);
            records.extend(8u16.to_le_bytes());
            records.extend(pattern.to_le_bytes());
            records.extend(count.to_le_bytes());
        }
        let mut data = ((records.len() + 4) as u32).to_le_bytes().to_vec();
        data.extend(records);
        data
    }
    fn chunk(start: u32, final_part: bool, pcm: &[i16]) -> Vec<u8> {
        wire(start, true, final_part, Some(pcm), 9997, None)
    }
    fn samples(pcm: &Pcm) -> Vec<i16> {
        pcm.iter().copied().collect()
    }
    #[test]
    fn decoded_store_state_and_effects_preserve_gapless_full_audio_until_recognition_ack() {
        use pebble_index::reception::{
            button_detector::Press,
            input_effects::{AudioStore, Effect, InputEffects},
            session_state::{Action, Observation, SessionState, SourceObservation},
        };
        let mut store = Recordings::new("integration-ring");
        let mut state = SessionState::default();
        let mut effects = InputEffects::default();
        let out = output();
        store.reset(10).unwrap();
        state
            .observe(
                0,
                Observation::Collecting {
                    active: true,
                    unread: 65546,
                },
            )
            .unwrap();
        let part = store
            .add(10, &chunk(10, false, &vec![1; 2000]), &out)
            .unwrap()
            .parts
            .remove(0);
        let key = part.key;
        let observation = |store: &Recordings| {
            let source = &store.sources[&key];
            Observation::Source(SourceObservation {
                id: key.clone(),
                first_collection: source.first,
                last_collection: *source.parts.keys().next_back().unwrap(),
                classification: Some(Press::Long),
                final_seen: source.final_at.is_some(),
                complete: source.complete(),
            })
        };
        let initial = state.observe(50, observation(&store)).unwrap();
        let session = initial.snapshot.session_id.unwrap();
        let plans = effects.reconcile(initial, &store, true).unwrap();
        let live = plans
            .iter()
            .find_map(|p| {
                if let Effect::Live(p) = p {
                    Some(p)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(live.pcm.len(), 2000);
        assert_eq!(
            live.pcm.slices().next().unwrap().as_ptr(),
            part.samples.slices().next().unwrap().as_ptr()
        );
        state
            .observe(
                100,
                Observation::Collecting {
                    active: false,
                    unread: 11,
                },
            )
            .unwrap();
        // final is retained immediately, but cannot make the missing middle complete.
        store.add(12, &chunk(10, true, &[]), &out).unwrap();
        let waiting = state.observe(160, observation(&store)).unwrap();
        assert!(
            waiting
                .actions
                .iter()
                .all(|a| !matches!(a, Action::Recognize { .. }))
        );
        assert!(!store.progress(&key).unwrap().complete);
        assert_eq!(store.progress(&key).unwrap().samples, 2000);
        effects.reconcile(waiting, &store, true).unwrap();
        store
            .add(11, &chunk(10, false, &vec![2; 999]), &out)
            .unwrap();
        let ended = state.observe(170, observation(&store)).unwrap();
        let plans = effects.reconcile(ended, &store, true).unwrap();
        let whole = plans
            .iter()
            .find_map(|p| {
                if let Effect::Batch(p) = p {
                    Some(p)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(whole.pcm.len(), 2999);
        assert_eq!(samples(&whole.pcm), [vec![1; 2000], vec![2; 999]].concat());
        assert_eq!(
            whole.pcm.slices().next().unwrap().as_ptr(),
            part.samples.slices().next().unwrap().as_ptr()
        );
        assert_eq!(store.collections, 3); // Queuing batch has not released receive ownership.
        let acknowledged = state
            .observe(180, Observation::Recognized(session))
            .unwrap();
        for effect in effects.reconcile(acknowledged, &store, true).unwrap() {
            if let Effect::Action(Action::Retire { sources, .. }) = effect {
                for source in sources {
                    store.release(&source);
                }
            }
        }
        assert_eq!(store.collections, 0);
        assert_eq!(whole.pcm.len(), 2999); // Shared consumer is still valid.
    }
    #[test]
    fn final_before_the_middle_waits_for_every_collection_and_keeps_empty_final() {
        let mut store = Recordings::new("ring");
        let out = output();
        let first = store
            .add(10, &chunk(10, false, &[1, 2]), &out)
            .unwrap()
            .parts
            .remove(0);
        let key = first.key;
        assert!(
            store
                .add(12, &wire(10, true, true, None, 0, Some((1, 1))), &out)
                .unwrap()
                .parts
                .is_empty()
        );
        assert!(store.whole(&key).is_err());
        assert_eq!(store.pending, 1);
        let ready = store.add(11, &chunk(10, false, &[3]), &out).unwrap().parts;
        assert_eq!(
            ready
                .iter()
                .map(|p| (p.index, p.final_part))
                .collect::<Vec<_>>(),
            [(11, false), (12, true)]
        );
        assert!(ready[1].samples.is_empty());
        assert_eq!(ready[1].rate, 9997);
        assert_eq!(samples(&store.whole(&key).unwrap()), [1, 2, 3]);
        assert_eq!(store.pending, 0);
    }
    #[test]
    fn duplicates_do_not_reopen_a_complete_or_acknowledged_source() {
        let mut store = Recordings::default();
        let out = output();
        let raw = chunk(2, true, &[23]);
        let part = store.add(2, &raw, &out).unwrap().parts.remove(0);
        assert!(store.add(2, &raw, &out).unwrap().parts.is_empty());
        assert_eq!(store.samples, 1);
        store.release(&part.key);
        assert!(store.sources.is_empty());
        assert!(store.add(2, &raw, &out).unwrap().parts.is_empty());
        assert_eq!(store.samples, 0);
        // Releasing the receive owner cannot invalidate an in-flight consumer.
        assert_eq!(samples(&part.samples), [23]);
    }
    #[test]
    fn store_and_consumers_share_the_same_pcm_and_keep_old_source_identity() {
        let mut store = Recordings::new("device-a");
        let out = output();
        let old = store
            .add(4, &chunk(4, false, &[1]), &out)
            .unwrap()
            .parts
            .remove(0);
        let new = store
            .add(6, &chunk(6, false, &[9]), &out)
            .unwrap()
            .parts
            .remove(0);
        let old_final = store
            .add(5, &chunk(4, true, &[2]), &out)
            .unwrap()
            .parts
            .remove(0);
        assert_eq!(old.key, old_final.key);
        assert_ne!(old.key, new.key);
        let whole = store.whole(&old.key).unwrap();
        assert_eq!(samples(&whole), [1, 2]);
        assert_eq!(
            whole.slices().next().unwrap().as_ptr(),
            old.samples.slices().next().unwrap().as_ptr()
        );
        assert!(store.whole(&new.key).is_err());
        let mut other = Recordings::new("device-a");
        let other = other
            .add(4, &chunk(4, false, &[1]), &out)
            .unwrap()
            .parts
            .remove(0);
        assert_ne!(old.key, other.key, "continuity epochs must not alias");
    }
    #[test]
    fn short_pcm_and_button_only_collections_are_preserved_for_classification() {
        let mut store = Recordings::default();
        let out = output();
        let a = store
            .add(
                1,
                &wire(1, false, true, Some(&[7]), 9997, Some((0, 1))),
                &out,
            )
            .unwrap()
            .parts
            .remove(0);
        assert_eq!(samples(&a.samples), [7]);
        assert_eq!(a.buttons.as_deref(), Some(["short".to_owned()].as_slice()));
        let b = store
            .add(2, &wire(2, false, true, None, 0, Some((0, 2))), &out)
            .unwrap()
            .parts
            .remove(0);
        assert!(b.final_part && b.samples.is_empty());
        assert_ne!(a.key, b.key);
    }
    #[test]
    fn metadata_prefix_does_not_invent_a_sample_rate() {
        let mut store = Recordings::default();
        let out = output();
        let prefix = store
            .add(9, &wire(9, true, false, None, 0, Some((0, 1))), &out)
            .unwrap()
            .parts
            .remove(0);
        let end = store
            .add(10, &chunk(9, true, &[5]), &out)
            .unwrap()
            .parts
            .remove(0);
        assert_eq!(prefix.key, end.key);
        assert_eq!(end.rate, 9997);
        assert_eq!(samples(&store.whole(&end.key).unwrap()), [5]);
    }
    #[test]
    fn conflicting_retransmission_final_and_rate_fail_without_mutation() {
        let mut store = Recordings::default();
        let out = output();
        let a = store
            .add(20, &chunk(20, false, &[1]), &out)
            .unwrap()
            .parts
            .remove(0);
        assert!(store.add(20, &chunk(20, false, &[2]), &out).is_err());
        assert!(
            store
                .add(21, &wire(20, true, true, Some(&[2]), 16000, None), &out)
                .is_err()
        );
        assert_eq!(store.collections, 1);
        assert_eq!(store.samples, 1);
        assert!(store.whole(&a.key).is_err());
        store.add(22, &chunk(20, false, &[3]), &out).unwrap();
        assert!(store.add(21, &chunk(20, true, &[2]), &out).is_err());
        assert_eq!(store.pending, 1);
        store.add(21, &chunk(20, false, &[2]), &out).unwrap();
        store.add(23, &chunk(20, true, &[]), &out).unwrap();
        assert_eq!(samples(&store.whole(&a.key).unwrap()), [1, 2, 3]);
        assert!(store.add(24, &chunk(20, false, &[4]), &out).is_err());
    }
    #[test]
    fn bounds_are_explicit_and_insertion_can_retry_after_acknowledgement() {
        let mut store = Recordings {
            limits: Limits {
                samples: 2,
                sources: 1,
                collections: 2,
                pending: 1,
            },
            ..Recordings::default()
        };
        let out = output();
        let a = store
            .add(1, &chunk(1, false, &[1, 2]), &out)
            .unwrap()
            .parts
            .remove(0);
        assert!(store.add(2, &chunk(1, false, &[3]), &out).is_err());
        assert_eq!(store.samples, 2);
        store.add(2, &chunk(1, true, &[]), &out).unwrap();
        assert!(store.add(3, &chunk(3, true, &[4]), &out).is_err());
        store.release(&a.key);
        let b = store.add(3, &chunk(3, true, &[4]), &out).unwrap().parts;
        assert_eq!(samples(&b[0].samples), [4]);
    }
    #[test]
    fn pending_limit_still_allows_the_missing_contiguous_prefix() {
        let mut store = Recordings::default();
        store.limits.pending = 1;
        let out = output();
        assert!(
            store
                .add(2, &chunk(0, true, &[3]), &out)
                .unwrap()
                .parts
                .is_empty()
        );
        assert!(store.add(1, &chunk(0, false, &[2]), &out).is_err());
        assert_eq!(
            store
                .add(0, &chunk(0, false, &[1]), &out)
                .unwrap()
                .parts
                .len(),
            1
        );
        let rest = store.add(1, &chunk(0, false, &[2]), &out).unwrap().parts;
        assert_eq!(rest.len(), 2);
        assert!(rest[1].final_part);
        assert_eq!(store.pending, 0);
    }
    #[test]
    fn ring_eviction_reports_loss_once_without_discarding_the_retained_audio() {
        let mut store = Recordings::default();
        let out = output();
        let a = store
            .add(10, &chunk(10, false, &[1]), &out)
            .unwrap()
            .parts
            .remove(0);
        store.add(12, &chunk(10, true, &[3]), &out).unwrap();
        assert_eq!(
            store.retain(12, 13, &out).unwrap().lost,
            std::slice::from_ref(&a.key)
        );
        assert!(store.retain(12, 13, &out).unwrap().lost.is_empty());
        assert_eq!(store.samples, 2);
        assert!(store.whole(&a.key).is_err());
        assert!(store.reset(13).is_err());
        store.release(&a.key);
        assert_eq!((store.samples, store.collections, store.pending), (0, 0, 0));
        store.reset(13).unwrap();
    }
    #[test]
    fn u16_wrap_does_not_alias_single_collections_or_erase_the_startup_boundary() {
        let mut store = Recordings::default();
        let out = output();
        store.reset(0).unwrap();
        let first = store
            .add(0, &wire(0, false, true, Some(&[1]), 9997, None), &out)
            .unwrap()
            .parts
            .remove(0);
        for index in [32767, 65534] {
            store
                .add(
                    index,
                    &wire(index as u32, false, true, Some(&[2]), 9997, None),
                    &out,
                )
                .unwrap();
        }
        let next_cycle = store
            .add(0, &wire(65536, false, true, Some(&[3]), 9997, None), &out)
            .unwrap()
            .parts
            .remove(0);
        assert_ne!(first.key, next_cycle.key);
        assert_eq!(samples(&store.whole(&first.key).unwrap()), [1]);
        assert_eq!(samples(&store.whole(&next_cycle.key).unwrap()), [3]);
    }
}
