//! Bounded recognition sessions. No transport, threads, UI, or input gestures.
use crate::qwen::{
    audio::{RATE, boundary, mean_square},
    inference::{AsrInference, TranscribeResult, WindowCache},
};
use anyhow::{Result, ensure};

pub trait Decoder {
    fn decode(
        &self,
        audio: &[f32],
        language: &str,
        prefix: &[i64],
        cache: &mut WindowCache,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<TranscribeResult>;
    fn rollback(&self, tokens: &[i64], count: usize) -> Result<Vec<i64>>;
}
impl Decoder for AsrInference {
    fn decode(
        &self,
        audio: &[f32],
        language: &str,
        prefix: &[i64],
        cache: &mut WindowCache,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<TranscribeResult> {
        self.transcribe_cached(audio, Some(language), prefix, cache, cancelled)
    }
    fn rollback(&self, tokens: &[i64], count: usize) -> Result<Vec<i64>> {
        self.rollback_prefix(tokens, count)
    }
}
#[derive(Debug, Clone)]
pub struct Update {
    pub text: String,
    pub consumed_samples: u64,
    pub buffered_samples: usize,
    pub segment: Option<(u64, u64)>,
}
fn join(left: &str, right: &str, language: &str) -> String {
    let (left, right) = (left.trim(), right.trim());
    if left.is_empty() {
        return right.into();
    }
    if right.is_empty() {
        return left.into();
    }
    let sep = if matches!(language, "Japanese" | "Chinese" | "Cantonese" | "Thai") {
        ""
    } else {
        " "
    };
    format!("{left}{sep}{right}")
}

pub struct LiveSession {
    language: String,
    window: Vec<f32>,
    cache: WindowCache,
    prefix: Vec<i64>,
    turns: usize,
    committed: String,
    current: String,
    offset: u64,
    last_decoded: usize,
    finished: bool,
    max_samples: usize,
    interval_samples: usize,
}
impl LiveSession {
    pub fn new(language: String) -> Self {
        Self {
            language,
            window: Vec::new(),
            cache: WindowCache::default(),
            prefix: Vec::new(),
            turns: 0,
            committed: String::new(),
            current: String::new(),
            offset: 0,
            last_decoded: 0,
            finished: false,
            max_samples: 30 * RATE,
            interval_samples: RATE,
        }
    }
    fn decode(
        &mut self,
        model: &impl Decoder,
        control: &dyn Fn() -> bool,
        extra_rollback: usize,
    ) -> Result<()> {
        if self.window.len() < 400
            || (self.prefix.is_empty()
                && self.current.is_empty()
                && mean_square(&self.window) < 1e-6)
        {
            self.current.clear();
            self.last_decoded = self.window.len();
            return Ok(());
        }
        let prefix = if self.turns >= 2 {
            model.rollback(&self.prefix, 5 + extra_rollback)?
        } else {
            Vec::new()
        };
        let result = model.decode(
            &self.window,
            &self.language,
            &prefix,
            &mut self.cache,
            control,
        )?;
        self.current = result.text;
        self.prefix = result.token_ids;
        self.turns += 1;
        self.last_decoded = self.window.len();
        Ok(())
    }
    pub fn feed(
        &mut self,
        model: &impl Decoder,
        audio: &[f32],
        final_input: bool,
        control: &dyn Fn() -> bool,
    ) -> Result<Vec<Update>> {
        ensure!(!self.finished, "Live session already finished");
        ensure!(audio.iter().all(|v| v.is_finite()), "Non-finite PCM");
        let mut updates = Vec::new();
        let mut input = audio;
        while !input.is_empty() {
            ensure!(!control(), "Recognition cancelled");
            if self.window.len() == self.max_samples {
                let cut = boundary(&self.window);
                let carry = self.window.split_off(cut);
                // Reuse the stable encoder/KV prefix. Roll back a generous token
                // budget for audio removed from the last decoded window.
                let rollback = (self.last_decoded.saturating_sub(cut) * 24).div_ceil(RATE);
                if self.last_decoded != cut {
                    self.decode(model, control, rollback)?;
                }
                self.committed = join(&self.committed, &self.current, &self.language);
                let start = self.offset;
                self.offset += cut as u64;
                updates.push(Update {
                    text: self.committed.clone(),
                    consumed_samples: self.offset,
                    buffered_samples: carry.len(),
                    segment: Some((start, self.offset)),
                });
                self.window = carry;
                self.current.clear();
                self.prefix.clear();
                self.turns = 0;
                self.last_decoded = 0;
                self.cache.reset();
            }
            let n = (self.max_samples - self.window.len()).min(input.len());
            self.window.extend_from_slice(&input[..n]);
            input = &input[n..];
        }
        if final_input
            || self.window.len().saturating_sub(self.last_decoded) >= self.interval_samples
        {
            ensure!(!control(), "Recognition cancelled");
            if self.window.len() != self.last_decoded {
                self.decode(model, control, 0)?;
            }
            updates.push(Update {
                text: join(&self.committed, &self.current, &self.language),
                consumed_samples: self.offset + self.window.len() as u64,
                buffered_samples: self.window.len(),
                segment: None,
            });
        }
        self.finished = final_input;
        Ok(updates)
    }
    pub fn buffered_samples(&self) -> usize {
        self.window.len()
    }
}

pub struct BatchSession {
    language: String,
    pending: Vec<f32>,
    offset: u64,
    text: String,
    finished: bool,
}
impl BatchSession {
    pub fn new(language: String) -> Self {
        Self {
            language,
            pending: Vec::new(),
            offset: 0,
            text: String::new(),
            finished: false,
        }
    }
    pub fn feed(
        &mut self,
        model: &impl Decoder,
        audio: &[f32],
        final_input: bool,
        control: &dyn Fn() -> bool,
    ) -> Result<Vec<Update>> {
        ensure!(!self.finished, "Batch session already finished");
        ensure!(audio.iter().all(|v| v.is_finite()), "Non-finite PCM");
        let mut updates = Vec::new();
        let mut input = audio;
        loop {
            let n = (30 * RATE - self.pending.len()).min(input.len());
            self.pending.extend_from_slice(&input[..n]);
            input = &input[n..];
            let full = self.pending.len() == 30 * RATE;
            if !(full || final_input && input.is_empty() && !self.pending.is_empty()) {
                break;
            }
            ensure!(!control(), "Recognition cancelled");
            let cut = if full {
                boundary(&self.pending)
            } else {
                self.pending.len()
            };
            let audio = &self.pending[..cut];
            if audio.len() >= 400 && mean_square(audio) >= 1e-6 {
                let result = model.decode(
                    audio,
                    &self.language,
                    &[],
                    &mut WindowCache::default(),
                    control,
                )?;
                self.text = join(&self.text, &result.text, &self.language);
            }
            let start = self.offset;
            self.offset += cut as u64;
            self.pending.drain(..cut);
            updates.push(Update {
                text: self.text.clone(),
                consumed_samples: self.offset,
                buffered_samples: self.pending.len(),
                segment: Some((start, self.offset)),
            });
        }
        if final_input {
            ensure!(self.pending.is_empty(), "Unprocessed final PCM");
            self.finished = true;
            if updates.is_empty() {
                updates.push(Update {
                    text: self.text.clone(),
                    consumed_samples: self.offset,
                    buffered_samples: 0,
                    segment: None,
                });
            }
        }
        Ok(updates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    #[derive(Default)]
    struct Fake {
        audio: RefCell<Vec<Vec<f32>>>,
        prefixes: RefCell<Vec<Vec<i64>>>,
    }
    impl Decoder for Fake {
        fn decode(
            &self,
            audio: &[f32],
            _: &str,
            prefix: &[i64],
            _: &mut WindowCache,
            cancel: &dyn Fn() -> bool,
        ) -> Result<TranscribeResult> {
            ensure!(!cancel(), "cancelled");
            self.audio.borrow_mut().push(audio.to_vec());
            self.prefixes.borrow_mut().push(prefix.to_vec());
            Ok(TranscribeResult {
                text: "確認。".into(),
                language: "Japanese".into(),
                raw_output: String::new(),
                duration_seconds: audio.len() as f64 / RATE as f64,
                token_ids: (0..10).collect(),
                timings: Default::default(),
            })
        }
        fn rollback(&self, tokens: &[i64], n: usize) -> Result<Vec<i64>> {
            Ok(tokens[..tokens.len().saturating_sub(n)].to_vec())
        }
    }
    #[test]
    fn quiet_tail_does_not_erase_recognized_speech() {
        let model = Fake::default();
        let mut live = LiveSession::new("Japanese".into());
        live.feed(&model, &vec![0.002; RATE], false, &|| false)
            .unwrap();
        let updates = live
            .feed(&model, &vec![0.0; 29 * RATE], true, &|| false)
            .unwrap();
        assert_eq!(updates.last().unwrap().text, "確認。");
        assert_eq!(model.audio.borrow().len(), 2);
    }
    #[test]
    fn batch_covers_every_sample_once_and_joins_japanese_without_spaces() {
        let model = Fake::default();
        let mut batch = BatchSession::new("Japanese".into());
        let audio: Vec<f32> = (0..RATE * 95 + 123)
            .map(|i| 0.01 + (i % 97) as f32 / 1000.0)
            .collect();
        let mut spans = Vec::new();
        let mut last = String::new();
        for chunk in audio.chunks(3217) {
            for u in batch.feed(&model, chunk, false, &|| false).unwrap() {
                spans.push(u.segment.unwrap());
                last = u.text;
            }
        }
        for u in batch.feed(&model, &[], true, &|| false).unwrap() {
            spans.push(u.segment.unwrap());
            last = u.text;
        }
        assert_eq!(model.audio.borrow().concat(), audio);
        assert!(model.audio.borrow().iter().all(|v| v.len() <= RATE * 30));
        assert_eq!(spans.first().unwrap().0, 0);
        assert_eq!(spans.last().unwrap().1, audio.len() as u64);
        assert!(spans.windows(2).all(|p| p[0].1 == p[1].0));
        assert_eq!(last, "確認。確認。確認。確認。");
    }
    #[test]
    fn live_is_bounded_and_resets_prefix_at_window_boundaries() {
        let model = Fake::default();
        let mut live = LiveSession::new("Japanese".into());
        for _ in 0..200 {
            live.feed(&model, &vec![0.1; RATE], false, &|| false)
                .unwrap();
            assert!(live.buffered_samples() <= RATE * 30);
        }
        let final_update = live
            .feed(&model, &[], true, &|| false)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(final_update.consumed_samples, 200 * RATE as u64);
        assert!(model.audio.borrow().iter().all(|v| v.len() <= 30 * RATE));
        assert!(model.prefixes.borrow().iter().any(|v| v.len() == 5));
        let fresh = Fake::default();
        LiveSession::new("Japanese".into())
            .feed(&fresh, &vec![0.1; RATE], true, &|| false)
            .unwrap();
        assert!(fresh.prefixes.borrow()[0].is_empty());
    }
    #[test]
    fn quiet_audio_is_empty_and_cancellation_propagates() {
        let model = Fake::default();
        let u = BatchSession::new("Japanese".into())
            .feed(&model, &vec![0.0; RATE * 61], true, &|| false)
            .unwrap();
        assert!(u.last().unwrap().text.is_empty());
        assert!(model.audio.borrow().is_empty());
        assert!(
            LiveSession::new("Japanese".into())
                .feed(&model, &[0.1; 1000], true, &|| true)
                .is_err()
        );
    }
    #[test]
    fn latin_word_boundaries_are_preserved() {
        assert_eq!(join("quiet.", "Today", "English"), "quiet. Today");
    }
}
