use super::model::Phase;
use super::settings::Presentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Hidden,
    Circle,
    Transcript,
}

pub fn surface(options: Presentation, phase: Option<Phase>, error: bool) -> Surface {
    if error {
        return Surface::Transcript;
    }
    match phase {
        None => Surface::Hidden,
        Some(Phase::Recording | Phase::Receiving) if !options.live_text => Surface::Circle,
        Some(Phase::Finalizing) if !options.live_text || !options.final_text => Surface::Circle,
        Some(_) => Surface::Transcript,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_visibility_combinations_preserve_processing_indicators() {
        for live_text in [false, true] {
            for final_text in [false, true] {
                let p = Presentation {
                    live_text,
                    final_text,
                };
                for phase in [Phase::Recording, Phase::Receiving] {
                    assert_eq!(
                        surface(p, Some(phase), false),
                        if live_text {
                            Surface::Transcript
                        } else {
                            Surface::Circle
                        }
                    );
                }
                assert_eq!(
                    surface(p, Some(Phase::Finalizing), false),
                    if live_text && final_text {
                        Surface::Transcript
                    } else {
                        Surface::Circle
                    }
                );
                assert_eq!(surface(p, None, false), Surface::Hidden);
                assert_eq!(surface(p, None, true), Surface::Transcript);
                assert_eq!(surface(p, Some(Phase::Ready), false), Surface::Transcript);
            }
        }
    }
}
