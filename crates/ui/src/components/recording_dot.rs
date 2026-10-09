use super::pulse;
use crate::theme::ACCENT;
use gpui::{
    Animation, AnimationExt, App, BoxShadow, IntoElement, RenderOnce, Styled, Window, div, point,
    px, rgb, rgba,
};
use std::time::Duration;

/// Pulsing red dot shown beside a transcript while recording.
#[derive(IntoElement)]
pub struct RecordingDot {
    reduce_motion: bool,
}
impl RecordingDot {
    pub fn new(reduce_motion: bool) -> Self {
        Self { reduce_motion }
    }
}
impl RenderOnce for RecordingDot {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let reduce_motion = self.reduce_motion;
        div()
            .size(px(15.))
            .rounded_full()
            .bg(rgb(ACCENT))
            .with_animation(
                "recording-glow",
                Animation::new(Duration::from_millis(1067)).repeat(),
                move |d, delta| {
                    let alpha = if reduce_motion {
                        0.65
                    } else {
                        0.12 + 0.53 * pulse(delta)
                    };
                    d.shadow(vec![BoxShadow {
                        color: rgba((ACCENT << 8) | (alpha * 255.) as u32).into(),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(4.),
                        spread_radius: px(1.5),
                    }])
                },
            )
    }
}
