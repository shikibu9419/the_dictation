use crate::{
    collection::{Collection, decode},
    output::Output,
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, HashMap};

pub struct Part {
    pub key: String,
    pub samples: Vec<i16>,
    pub rate: u32,
    pub final_part: bool,
    pub next: u16,
}
struct Recording {
    next: u16,
    rate: Option<u32>,
    pending: BTreeMap<u16, Collection>,
}
#[derive(Default)]
pub struct Recordings {
    parts: HashMap<u32, Recording>,
    boundary: Option<u16>,
}
impl Recordings {
    pub fn reset(&mut self, index: u16) {
        self.parts.clear();
        self.boundary = Some(index);
    }
    pub fn add(&mut self, index: u16, raw: &[u8], output: &Output) -> Result<Vec<Part>> {
        let item = decode(raw)?;
        output.debug(format!(
            "collection={index} bytes={} multipart={} final={} recording_start={:?}",
            raw.len(),
            item.multipart,
            item.final_part,
            item.start
        ));
        if let (Some(samples), Some(rate)) = (&item.samples, item.rate) {
            output.debug(format!(
                "audio collection={index} samples={} rate={rate} duration={:.3}s buttons={:?}",
                samples.len(),
                samples.len() as f64 / rate as f64,
                item.buttons
            ));
        }
        if !item.multipart {
            // Button-only collections can carry a few dummy PCM samples.
            // These are not utterances and must not create another UI item or
            // feed padded silence into the recognizer.
            if let (Some(samples), Some(rate)) = (&item.samples, item.rate)
                && samples.len() * 1000 < rate as usize * 150
            {
                output.debug(format!("Empty result for short collection={index}: {} samples (<150ms), buttons={:?}", samples.len(), item.buttons));
                return Ok(vec![Part {
                    key: format!("({index}, {:?})", item.start),
                    samples: vec![], rate, final_part: true,
                    next: index.wrapping_add(1),
                }]);
            }
            return Ok(match (item.samples, item.rate) {
                (Some(samples), Some(rate)) => vec![Part {
                    key: format!("({index}, {:?})", item.start),
                    samples,
                    rate,
                    final_part: true,
                    next: index.wrapping_add(1),
                }],
                _ => vec![],
            });
        }
        let key = item
            .start
            .context("Multipart recording lacks starting collection count")?;
        let first = key as u16;
        if self
            .boundary
            .is_some_and(|boundary| first.wrapping_sub(boundary) >= 32768)
        {
            output.debug(format!(
                "Skipping pre-start recording={key} collection={index}"
            ));
            return Ok(vec![]);
        }
        self.boundary = None;
        let recording = self.parts.entry(key).or_insert_with(|| Recording {
            next: first,
            rate: None,
            pending: BTreeMap::new(),
        });
        if index.wrapping_sub(recording.next) >= 32768 {
            return Ok(vec![]);
        }
        recording.pending.insert(index, item);
        let mut result = Vec::new();
        let mut done = false;
        while let Some(part) = recording.pending.remove(&recording.next) {
            if part.samples.is_some() {
                if let Some(rate) = recording.rate {
                    ensure!(
                        Some(rate) == part.rate,
                        "Sample rate changed within recording"
                    );
                }
                recording.rate = part.rate;
            }
            recording.next = recording.next.wrapping_add(1);
            if let Some(rate) = recording.rate {
                result.push(Part {
                    key: key.to_string(),
                    samples: part.samples.unwrap_or_default(),
                    rate,
                    final_part: part.final_part,
                    next: recording.next,
                });
            }
            if part.final_part {
                done = true;
                break;
            }
        }
        if done {
            self.parts.remove(&key);
        }
        Ok(result)
    }
    pub fn retain(&mut self, start: u16, end: u16, output: &Output) -> Vec<String> {
        let count = end.wrapping_sub(start);
        let mut lost = Vec::new();
        self.parts.retain(|key,r| {
            if r.next!=end && r.next.wrapping_sub(start)>=count {
                output.error(format!("Recording {key} is incomplete: collection {} is no longer available in ring range {start}..{end}",r.next));
                lost.push(key.to_string()); false
            } else { true }
        });
        lost
    }
}
