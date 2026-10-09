//! Text measurement shared by panels that size themselves to their content.
use gpui::{Window, font, px, rgb};

/// Height of `text` laid out at `font_size`/`line_height` within `width`, clamped
/// to `min..=max`. Callers pass the same metrics they render with.
pub fn measure_body_height(
    window: &mut Window,
    text: &str,
    font_size: f32,
    line_height: f32,
    width: f32,
    min: f32,
    max: f32,
) -> f32 {
    let measured = window
        .text_system()
        .shape_text(
            text.to_owned().into(),
            px(font_size),
            &[gpui::TextRun {
                len: text.len(),
                font: font(crate::theme::FONT),
                color: rgb(crate::theme::TEXT).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            Some(px(width)),
            None,
        )
        .map(|lines| {
            lines
                .iter()
                .map(|line| f32::from(line.size(px(line_height)).height))
                .sum::<f32>()
        })
        .unwrap_or(line_height);
    measured.clamp(min, max)
}
