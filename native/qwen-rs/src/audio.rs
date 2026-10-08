//! PCM preprocessing independent of model inference and process I/O.
use anyhow::{ensure, Result};
pub const RATE: usize = 16_000;

pub struct Resampler {
    rate: u32,
    samples: Vec<f32>,
    base: u64,
    received: u64,
    emitted: u64,
    finished: bool,
}
impl Resampler {
    pub fn new(rate: u32) -> Result<Self> {
        ensure!((1000..=192000).contains(&rate), "Invalid PCM rate {rate}");
        Ok(Self {
            rate,
            samples: Vec::new(),
            base: 0,
            received: 0,
            emitted: 0,
            finished: false,
        })
    }
    pub fn feed(&mut self, pcm: &[f32], final_input: bool) -> Result<Vec<f32>> {
        ensure!(!self.finished, "Resampler already finished");
        ensure!(pcm.iter().all(|x| x.is_finite()), "Non-finite PCM");
        self.finished = final_input;
        self.received += pcm.len() as u64;
        if self.rate as usize == RATE {
            self.emitted += pcm.len() as u64;
            return Ok(pcm.to_vec());
        }
        self.samples.extend_from_slice(pcm);
        if self.samples.is_empty() {
            return Ok(Vec::new());
        }
        let stop = if final_input {
            let numerator = self.received * RATE as u64;
            let rate = self.rate as u64;
            let (n, rem) = (numerator / rate, numerator % rate);
            n + u64::from(rem * 2 > rate || (rem * 2 == rate && n % 2 != 0))
        } else {
            (self.received.saturating_sub(1) * RATE as u64)
                .div_ceil(self.rate as u64)
                .min(self.received * RATE as u64 / self.rate as u64)
        };
        let mut out = Vec::with_capacity(stop.saturating_sub(self.emitted) as usize);
        for i in self.emitted..stop {
            let numerator = i * self.rate as u64;
            let at = (numerator / RATE as u64 - self.base) as usize;
            let fraction = (numerator % RATE as u64) as f64 / RATE as f64;
            let left = self.samples[at.min(self.samples.len() - 1)] as f64;
            let right = self.samples[(at + 1).min(self.samples.len() - 1)] as f64;
            out.push((left + (right - left) * fraction) as f32);
        }
        self.emitted = stop;
        let keep = (stop * self.rate as u64 / RATE as u64).min(self.received - 1);
        self.samples.drain(..(keep - self.base) as usize);
        self.base = keep;
        Ok(out)
    }
}

/// Once opened, passes through everything, including silence and quiet endings.
#[derive(Default)]
pub struct SpeechStart {
    pending: Vec<f32>,
    cursor: usize,
    active: usize,
    started: bool,
}
impl SpeechStart {
    pub fn feed(&mut self, audio: &[f32]) -> Vec<f32> {
        if self.started {
            return audio.to_vec();
        }
        self.pending.extend_from_slice(audio);
        while self.cursor + 320 <= self.pending.len() {
            let energy = mean_square(&self.pending[self.cursor..self.cursor + 320]);
            self.active = if energy >= 1e-6 { self.active + 1 } else { 0 };
            self.cursor += 320;
            if self.active >= 3 {
                let start = self.cursor.saturating_sub(960 + 3200);
                let audio = self.pending.split_off(start);
                self.pending.clear();
                self.cursor = 0;
                self.started = true;
                return audio;
            }
        }
        // Keep pre-roll and any partly observed onset, preserving frame phase.
        let discard = self.cursor.saturating_sub(3840);
        self.pending.drain(..discard);
        self.cursor -= discard;
        Vec::new()
    }
}
pub fn mean_square(audio: &[f32]) -> f64 {
    if audio.is_empty() {
        return 0.0;
    }
    audio.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / audio.len() as f64
}

/// Prefer the centre of a sustained quiet run in the final three seconds.
/// This partitions disjoint sample intervals; no text-overlap guessing is used.
pub fn boundary(audio: &[f32]) -> usize {
    const FRAME: usize = 320;
    const HOP: usize = 160;
    if audio.len() < RATE * 4 {
        return audio.len();
    }
    let energies: Vec<f64> = audio.windows(FRAME).step_by(HOP).map(mean_square).collect();
    let mut sorted = energies.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    if median <= 1e-16 {
        return audio.len();
    }
    let start = (audio.len().saturating_sub(3 * RATE)).div_ceil(HOP);
    let mut run = None;
    let mut best: Option<(f64, usize)> = None;
    for i in start..=energies.len() {
        if i < energies.len() && energies[i] <= median * 0.25 {
            run.get_or_insert(i);
        } else if let Some(a) = run.take() {
            // At least 100ms, so a single stop-consonant closure is not a cut.
            if i - a >= 10 {
                let avg = energies[a..i].iter().sum::<f64>() / (i - a) as f64;
                let at = (a + i - 1) * HOP / 2 + FRAME / 2;
                if best.is_none_or(|(v, _)| avg <= v) {
                    best = Some((avg, at));
                }
            }
        }
    }
    best.map_or(audio.len(), |(_, at)| at.min(audio.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resampling_preserves_phase_across_arbitrary_packets() {
        for rate in [9997, 16000, 22050, 44100, 48000, 192000] {
            let input: Vec<f32> = (0..(rate as usize + 37))
                .map(|i| (i as f32 * 0.013).sin())
                .collect();
            let whole = Resampler::new(rate).unwrap().feed(&input, true).unwrap();
            let mut stream = Resampler::new(rate).unwrap();
            let mut split = Vec::new();
            for chunk in input.chunks(137) {
                split.extend(stream.feed(chunk, false).unwrap());
            }
            split.extend(stream.feed(&[], true).unwrap());
            assert_eq!(split, whole, "rate={rate}");
            assert!(stream.feed(&[], false).is_err());
        }
    }
    #[test]
    fn onset_retains_pre_roll_and_all_audio_after_opening() {
        let mut gate = SpeechStart::default();
        for _ in 0..100 {
            assert!(gate.feed(&[0.0; 137]).is_empty());
        }
        assert!(gate.feed(&[0.1; 640]).is_empty());
        let mut opened = gate.feed(&[0.1; 640]);
        opened.extend(gate.feed(&[0.0; 10000]));
        assert!(opened.len() >= 3200 + 1280 + 10000);
        assert_eq!(&opened[opened.len() - 10000..], &[0.0; 10000]);
    }
    #[test]
    fn clicks_and_silence_do_not_open_gate() {
        let mut gate = SpeechStart::default();
        let mut click = vec![0.0; 3200];
        click[50] = 1.0;
        assert!(gate.feed(&click).is_empty());
        assert!(gate.feed(&[0.0; 3200]).is_empty());
    }
    #[test]
    fn boundary_requires_sustained_quiet_and_makes_progress() {
        let mut audio = vec![0.1; RATE * 30];
        let a = RATE * 28;
        audio[a..a + 320].fill(0.0);
        assert_eq!(boundary(&audio), audio.len());
        audio[a..a + 3200].fill(0.0);
        let cut = boundary(&audio);
        assert!(cut > a && cut < a + 3200);
        assert_eq!(boundary(&vec![0.0; RATE * 30]), RATE * 30);
    }
}
