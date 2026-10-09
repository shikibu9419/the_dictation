/// RMS after removing DC, mapped from -55..-12 dBFS to a display envelope.
/// This is visual feedback, not voice-activity detection or an ASR gate.
#[cfg(test)]
pub fn normalized(samples: &[i16]) -> f64 {
    normalized_iter(samples.iter().copied())
}

pub fn normalized_iter(samples: impl Iterator<Item = i16> + Clone) -> f64 {
    let count = samples.clone().count();
    if count == 0 {
        return 0.;
    }
    let mean = samples.clone().map(|s| s as f64).sum::<f64>() / count as f64;
    let power = samples.map(|s| (s as f64 - mean).powi(2)).sum::<f64>() / count as f64;
    let rms = power.sqrt() / 32768.;
    if rms <= 0. {
        return 0.;
    }
    ((20. * rms.log10() + 55.) / 43.).clamp(0., 1.)
}

/// Display the newest 40 ms, not the RMS of up to a second of older audio.
pub fn latest(samples: &crate::pcm::Pcm, rate: u32) -> f64 {
    let count = (rate as usize * 40 / 1000).max(1);
    let tail = samples.range(samples.len().saturating_sub(count)..samples.len());
    normalized_iter(tail.iter().copied())
}

pub fn latest_slice(samples: &[i16], rate: u32) -> f64 {
    let count = (rate as usize * 40 / 1000).max(1);
    normalized_iter(
        samples[samples.len().saturating_sub(count)..]
            .iter()
            .copied(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_window_reacts_to_onsets_and_silence_without_averaging_old_audio() {
        let mut samples = vec![0; 960];
        samples.extend((0..40).map(|i| if i % 2 == 0 { 16000 } else { -16000 }));
        assert!(latest_slice(&samples, 1000) > 0.9);
        let pcm: crate::pcm::Pcm = samples.clone().into();
        assert_eq!(latest(&pcm, 1000), latest_slice(&samples, 1000));
        samples.extend([0; 40]);
        assert_eq!(latest_slice(&samples, 1000), 0.);
        assert_eq!(latest(&samples.into(), 1000), 0.);
    }
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
