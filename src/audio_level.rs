/// RMS after removing DC, mapped from -55..-12 dBFS to a display envelope.
/// This is visual feedback, not voice-activity detection or an ASR gate.
pub fn normalized(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.;
    }
    let mean = samples.iter().map(|&s| s as f64).sum::<f64>() / samples.len() as f64;
    let power = samples
        .iter()
        .map(|&s| (s as f64 - mean).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    let rms = power.sqrt() / 32768.;
    if rms <= 0. {
        return 0.;
    }
    ((20. * rms.log10() + 55.) / 43.).clamp(0., 1.)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_and_dc_do_not_light_up_the_meter() {
        assert_eq!(normalized(&[]), 0.);
        assert_eq!(normalized(&[0; 100]), 0.);
        assert_eq!(normalized(&[7000; 100]), 0.);
    }
    #[test]
    fn amplitude_is_monotonic_bounded_and_signed_safe() {
        let levels: Vec<_> = [1, 100, 1000, 10000, 32767]
            .into_iter()
            .map(|n| normalized(&[-n, n]))
            .collect();
        assert!(levels.windows(2).all(|p| p[0] <= p[1]));
        assert!(levels.iter().all(|n| (0. ..=1.).contains(n)));
        assert_eq!(normalized(&[i16::MIN, i16::MAX]), 1.);
    }
}
