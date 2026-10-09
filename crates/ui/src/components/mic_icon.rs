use super::pulse;
use crate::theme::ACCENT;
use gpui::{
    Animation, AnimationExt, App, BoxShadow, IntoElement, PathBuilder, RenderOnce, Styled, Window,
    canvas, div, point, px, rgb, rgba,
};
use std::time::Duration;

/// Red microphone glyph with a breathing glow.
#[derive(IntoElement)]
pub struct MicIcon {
    reduce_motion: bool,
}
impl MicIcon {
    pub fn new(reduce_motion: bool) -> Self {
        Self { reduce_motion }
    }
}
impl RenderOnce for MicIcon {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let reduce_motion = self.reduce_motion;
        div().size(px(24.)).rounded_full().with_animation(
            "microphone-glow",
            Animation::new(Duration::from_millis(1067)).repeat(),
            move |d, delta| {
                let alpha = if reduce_motion {
                    0.35
                } else {
                    0.12 + 0.28 * pulse(delta)
                };
                d.shadow(vec![BoxShadow {
                    color: rgba((ACCENT << 8) | (alpha * 255.) as u32).into(),
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(7.),
                    spread_radius: px(0.),
                }])
                .child(
                    canvas(
                        |_, _, _| (),
                        |bounds, _, window, _| {
                            let center = bounds.center();
                            let mut path = PathBuilder::stroke(px(2.2));
                            let p = |x: f32, y: f32| center + point(px(x), px(y));
                            path.move_to(p(-4., -7.));
                            path.cubic_bezier_to(p(4., -7.), p(-4., -12.), p(4., -12.));
                            path.line_to(p(4., 1.));
                            path.cubic_bezier_to(p(-4., 1.), p(4., 6.), p(-4., 6.));
                            path.close();
                            path.move_to(p(-7., -1.));
                            path.line_to(p(-7., 1.));
                            path.cubic_bezier_to(p(7., 1.), p(-7., 10.), p(7., 10.));
                            path.line_to(p(7., -1.));
                            path.move_to(p(0., 7.));
                            path.line_to(p(0., 11.));
                            path.move_to(p(-4., 11.));
                            path.line_to(p(4., 11.));
                            if let Ok(path) = path.build() {
                                window.paint_path(path, rgb(ACCENT));
                            }
                        },
                    )
                    .size_full(),
                )
            },
        )
    }
}
