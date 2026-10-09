use crate::theme::{FONT, TEXT};
use gpui::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px, rgb, rgba,
};

/// Rounded glass frame of a floating panel: a fixed indicator slot on the
/// left and an optional body. `circular` collapses it to the indicator alone.
#[derive(IntoElement)]
pub struct PanelSurface {
    circular: bool,
    indicator: Option<AnyElement>,
    body: Option<AnyElement>,
}
impl PanelSurface {
    pub fn new(circular: bool) -> Self {
        Self {
            circular,
            indicator: None,
            body: None,
        }
    }
    pub fn indicator(mut self, indicator: impl IntoElement) -> Self {
        self.indicator = Some(indicator.into_any_element());
        self
    }
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }
}
impl RenderOnce for PanelSurface {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let circular = self.circular;
        let slot = div()
            .flex_none()
            .w(px(21.))
            .h(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .children(self.indicator);
        div()
            .size_full()
            .p(px(if circular { 20. } else { 10. }))
            .font_family(FONT)
            .text_size(px(22.5))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .size_full()
                    .flex()
                    .items_start()
                    .py(px(if circular { 0. } else { 12. }))
                    .px(px(if circular { 0. } else { 16. }))
                    .gap(px(if circular { 0. } else { 10. }))
                    .when(circular, |d| d.items_center().justify_center())
                    .rounded(px(if circular { 28. } else { 18. }))
                    .bg(rgba(0x48484818))
                    .child(slot)
                    .children(if circular { None } else { self.body }),
            )
    }
}
