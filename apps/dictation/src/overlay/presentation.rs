use super::model::Phase;
use super::settings::Presentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Hidden,
    Circle,
    Transcript,
}

pub fn surface(options: Presentation, phase: Option<Phase>, error: bool, history: bool) -> Surface {
    if !options.live_mode && !history {
        return match (phase, error) {
            (None | Some(Phase::Ready | Phase::Failed), false) => Surface::Hidden,
            _ => Surface::Circle,
        };
    }
    if error {
        return Surface::Transcript;
    }
    match phase {
        None => Surface::Hidden,
        Some(Phase::Recording | Phase::Receiving) if !options.live_mode => Surface::Circle,
        Some(Phase::Finalizing) if !options.live_mode || !options.final_text => Surface::Circle,
        Some(_) => Surface::Transcript,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_live_mode_never_exposes_a_text_surface_outside_history() {
        let options = Presentation {
            live_mode: false,
            final_text: true,
        };
        for phase in [
            None,
            Some(Phase::Recording),
            Some(Phase::Receiving),
            Some(Phase::Reconnecting),
            Some(Phase::Finalizing),
            Some(Phase::Ready),
            Some(Phase::Failed),
        ] {
            for error in [false, true] {
                assert_ne!(surface(options, phase, error, false), Surface::Transcript);
            }
        }
        assert_eq!(
            surface(options, Some(Phase::Ready), false, false),
            Surface::Hidden
        );
        assert_eq!(
            surface(options, Some(Phase::Ready), false, true),
            Surface::Transcript
        );
    }
    #[test]
    fn all_visibility_combinations_preserve_processing_indicators() {
        for live_mode in [false, true] {
            for final_text in [false, true] {
                let p = Presentation {
                    live_mode,
                    final_text,
                };
                for phase in [Phase::Recording, Phase::Receiving] {
                    assert_eq!(
                        surface(p, Some(phase), false, false),
                        if live_mode {
                            Surface::Transcript
                        } else {
                            Surface::Circle
                        }
                    );
                }
                assert_eq!(
                    surface(p, Some(Phase::Finalizing), false, false),
                    if live_mode && final_text {
                        Surface::Transcript
                    } else {
                        Surface::Circle
                    }
                );
                assert_eq!(surface(p, None, false, false), Surface::Hidden);
                assert_eq!(
                    surface(p, None, true, false),
                    if live_mode {
                        Surface::Transcript
                    } else {
                        Surface::Circle
                    }
                );
                assert_eq!(
                    surface(p, Some(Phase::Ready), false, true),
                    Surface::Transcript
                );
            }
        }
    }
}
