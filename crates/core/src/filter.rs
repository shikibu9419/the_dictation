pub struct Filter {
    pub rate: u32,
    sections: [[f64; 5]; 2],
    state: [[f64; 2]; 2],
}
impl Filter {
    pub fn new(rate: u32) -> Self {
        let w = 2.0 * std::f64::consts::PI * 30.0 / rate as f64;
        let mut sections = [[0.0; 5]; 2];
        // SciPy's SOS ordering: low-Q section first, then high-Q.
        for (i, s) in sections.iter_mut().enumerate() {
            let q = 1.0 / (2.0 * (std::f64::consts::PI * (2 * i + 1) as f64 / 8.0).cos());
            let alpha = w.sin() / (2.0 * q);
            let a0 = 1.0 + alpha;
            let c = w.cos();
            *s = [
                (1.0 + c) / 2.0 / a0,
                -(1.0 + c) / a0,
                (1.0 + c) / 2.0 / a0,
                -2.0 * c / a0,
                (1.0 - alpha) / a0,
            ];
        }
        Self {
            rate,
            sections,
            state: [[0.0; 2]; 2],
        }
    }
    pub fn process(&mut self, samples: &[i16]) -> Vec<i16> {
        samples
            .iter()
            .map(|&sample| {
                let mut value = sample as f64;
                for (s, z) in self.sections.iter().zip(self.state.iter_mut()) {
                    let y = s[0] * value + z[0];
                    z[0] = s[1] * value - s[3] * y + z[1];
                    z[1] = s[2] * value - s[4] * y;
                    value = y;
                }
                value.round_ties_even().clamp(-32768.0, 32767.0) as i16
            })
            .collect()
    }
}
