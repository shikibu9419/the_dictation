use gpui::{App, Entity, ScrollHandle, SharedString, Window, div, prelude::*, px};
use gpui_component::input::{Input, InputState};

/// Transcript column: a read-only scrolling label, or the editor when an
/// `InputState` is supplied. The caller subscribes to the editor's events.
#[derive(IntoElement)]
pub struct Transcript {
    text: SharedString,
    editor: Option<Entity<InputState>>,
    scroll: Option<ScrollHandle>,
    body_height: f32,
    font_size: f32,
    line_height: f32,
    width: f32,
}
impl Transcript {
    pub fn new(text: impl Into<SharedString>, body_height: f32) -> Self {
        Self {
            text: text.into(),
            editor: None,
            scroll: None,
            body_height,
            font_size: 22.5,
            line_height: 30.,
            width: 487.,
        }
    }
    pub fn editor(mut self, editor: Entity<InputState>) -> Self {
        self.editor = Some(editor);
        self
    }
    pub fn scroll(mut self, scroll: ScrollHandle) -> Self {
        self.scroll = Some(scroll);
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }
    pub fn metrics(mut self, font_size: f32, line_height: f32) -> Self {
        self.font_size = font_size;
        self.line_height = line_height;
        self
    }
}
impl RenderOnce for Transcript {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let column = div().w(px(self.width + 10.)).flex_none().flex().flex_col();
        match self.editor {
            Some(editor) => column.child(
                Input::new(&editor)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .h(px(self.body_height))
                    .p_0()
                    .text_size(px(self.font_size))
                    .line_height(px(self.line_height)),
            ),
            None => {
                let label = div()
                    .id("transcript")
                    .w(px(self.width))
                    .h(px(self.body_height))
                    .overflow_y_scroll()
                    .line_height(px(self.line_height))
                    .child(self.text);
                column.child(match self.scroll {
                    Some(scroll) => label.track_scroll(&scroll),
                    None => label,
                })
            }
        }
    }
}
