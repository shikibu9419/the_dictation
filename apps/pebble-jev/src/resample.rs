//! Streaming windowed-sinc resampler for mono PCM16. Sized for speech: the
//! ring's 9997 Hz and the Realtime API's 24 kHz.

const TAPS: usize = 16;

pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Low-pass scale relative to the input rate; 1.0 when upsampling.
    scale: f64,
    history: Vec<f32>,
    /// Fractional input index of the next output sample within `history`.
    position: f64,
}
impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        let mut resampler = Self {
            step: from_rate as f64 / to_rate as f64,
            scale: (to_rate as f64 / from_rate as f64).min(1.0),
            history: Vec::new(),
            position: 0.,
        };
        resampler.reset();
        resampler
    }
    pub fn reset(&mut self) {
        self.history.clear();
        self.history.resize(TAPS, 0.);
        self.position = TAPS as f64;
    }
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        self.history
            .extend(input.iter().map(|&s| s as f32 / 32768.));
        self.drain()
    }
    /// Emit the samples still waiting on future input, then reset.
    pub fn flush(&mut self) -> Vec<i16> {
        self.history.extend(std::iter::repeat_n(0., TAPS));
        let tail = self.drain();
        self.reset();
        tail
    }
    fn drain(&mut self) -> Vec<i16> {
        let mut out = Vec::new();
        while self.position + TAPS as f64 <= self.history.len() as f64 {
            out.push(self.sample(self.position));
            self.position += self.step;
        }
        let consumed = (self.position.floor() as usize).saturating_sub(TAPS);
        if consumed > 0 {
            self.history.drain(..consumed);
            self.position -= consumed as f64;
        }
        out
    }
    fn sample(&self, t: f64) -> i16 {
        let center = t.floor() as isize;
        let mut acc = 0.;
        let mut weight = 0.;
        for k in (center - TAPS as isize + 1)..=(center + TAPS as isize) {
            let u = t - k as f64;
            let window = 0.5 + 0.5 * (std::f64::consts::PI * u / TAPS as f64).cos();
            let h = sinc(self.scale * u) * window;
            weight += h;
            if let Some(&x) = usize::try_from(k).ok().and_then(|k| self.history.get(k)) {
                acc += x as f64 * h;
            }
        }
        let value = if weight.abs() > 1e-9 {
            acc / weight
        } else {
            0.
        };
        (value * 32767.).round().clamp(-32768., 32767.) as i16
    }
}
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|n| {
                (0.5 * (2. * std::f64::consts::PI * hz * n as f64 / rate as f64).sin() * 32767.)
                    as i16
            })
            .collect()
    }

    #[test]
    fn ring_rate_to_realtime_rate_keeps_length_ratio() {
        let mut resampler = Resampler::new(9997, 24000);
        let mut out = Vec::new();
        for chunk in sine(9997, 440., 9997).chunks(1999) {
            out.extend(resampler.process(chunk));
        }
        out.extend(resampler.flush());
        let expected = 24000;
        assert!((out.len() as i64 - expected).abs() < 40, "{}", out.len());
    }

    #[test]
    fn tone_is_preserved_across_chunk_boundaries() {
        let mut resampler = Resampler::new(9997, 24000);
        let mut out = Vec::new();
        for chunk in sine(9997, 440., 4000).chunks(333) {
            out.extend(resampler.process(chunk));
        }
        out.extend(resampler.flush());
        // Skip the filter delay at both ends, then compare with the ideal tone.
        let delay = (TAPS as f64 * 24000. / 9997.) as usize;
        let body = &out[delay..out.len() - 2 * delay];
        let reference = sine(24000, 440., out.len());
        let worst = body
            .iter()
            .zip(&reference[delay..])
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(worst < 1200, "max deviation {worst}");
    }

    #[test]
    fn downsampling_scales_the_low_pass() {
        let mut resampler = Resampler::new(24000, 16000);
        let out = resampler.process(&sine(24000, 300., 2400));
        assert!(out.len() > 1500 && out.len() < 1600, "{}", out.len());
    }

    #[test]
    fn reset_forgets_history() {
        let mut resampler = Resampler::new(16000, 24000);
        resampler.process(&[20000; 500]);
        resampler.reset();
        let out = resampler.process(&[0; 100]);
        assert!(out.iter().all(|s| s.abs() < 2));
    }
}
