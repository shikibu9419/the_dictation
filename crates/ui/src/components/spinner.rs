use gpui::{
    Animation, AnimationExt, App, PathBuilder, Window, canvas, div, point, prelude::*, px, rgba,
};
use std::time::Duration;

/// Rotating ring of fading segments.
#[derive(IntoElement)]
pub struct Spinner {
    reduce_motion: bool,
}
impl Spinner {
    pub fn new(reduce_motion: bool) -> Self {
        Self { reduce_motion }
    }
}
impl RenderOnce for Spinner {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let reduce_motion = self.reduce_motion;
        div().size(px(21.)).with_animation(
            "processing",
            Animation::new(Duration::from_millis(850)).repeat(),
            move |d, delta| {
                let delta = if reduce_motion { 0. } else { delta };
                d.child(
                    canvas(
                        |_, _, _| (),
                        move |bounds, _, window, _| {
                            for n in 0..48 {
                                let mut path = PathBuilder::stroke(px(3.));
                                for step in 0..=2 {
                                    let angle = (delta + (n as f32 + step as f32 / 2.) / 56.)
                                        * std::f32::consts::TAU;
                                    let p = bounds.center()
                                        + point(px(angle.cos() * 8.25), px(angle.sin() * 8.25));
                                    if step == 0 {
                                        path.move_to(p);
                                    } else {
                                        path.line_to(p);
                                    }
                                }
                                if let Ok(path) = path.build() {
                                    let alpha = (20. + 235. * (n as f32 / 47.).powf(1.5)) as u32;
                                    window.paint_path(path, rgba(0xe5e5e500 | alpha));
                                }
                            }
                        },
                    )
                    .size_full(),
                )
            },
        )
    }
}
