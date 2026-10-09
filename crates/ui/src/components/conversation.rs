use gpui::{
    App, IntoElement, ParentElement, RenderOnce, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px, rgb, rgba,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
    Failed,
}

/// One entry of a spoken conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Turn {
    User {
        text: SharedString,
        streaming: bool,
    },
    Assistant {
        text: SharedString,
        streaming: bool,
    },
    ToolCall {
        name: SharedString,
        arguments: SharedString,
        result: Option<SharedString>,
        status: ToolStatus,
    },
}

/// Scrolling list of turns. Streaming turns end with an ellipsis so partial
/// text reads as in progress.
#[derive(IntoElement)]
pub struct Conversation {
    turns: Vec<Turn>,
    scroll: Option<ScrollHandle>,
    body_height: f32,
    width: f32,
}
impl Conversation {
    pub fn new(turns: Vec<Turn>, body_height: f32) -> Self {
        Self {
            turns,
            scroll: None,
            body_height,
            width: 487.,
        }
    }
    pub fn scroll(mut self, scroll: ScrollHandle) -> Self {
        self.scroll = Some(scroll);
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }
}
fn streaming_text(text: &SharedString, streaming: bool) -> SharedString {
    if streaming {
        format!("{text}…").into()
    } else {
        text.clone()
    }
}
impl RenderOnce for Conversation {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let list = div()
            .id("conversation")
            .w(px(self.width))
            .h(px(self.body_height))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(8.))
            .line_height(px(30.))
            .children(self.turns.into_iter().map(|turn| {
                match turn {
                    Turn::User { text, streaming } => div()
                        .text_color(rgb(0xa9b4c2))
                        .child(streaming_text(&text, streaming))
                        .into_any_element(),
                    Turn::Assistant { text, streaming } => div()
                        .child(streaming_text(&text, streaming))
                        .into_any_element(),
                    Turn::ToolCall {
                        name,
                        arguments,
                        result,
                        status,
                    } => {
                        let tint = match status {
                            ToolStatus::Running => 0xe5c07b,
                            ToolStatus::Done => 0x8be9a4,
                            ToolStatus::Failed => 0xff6b7a,
                        };
                        div()
                            .rounded(px(10.))
                            .bg(rgba(0xffffff14))
                            .px(px(12.))
                            .py(px(8.))
                            .text_size(px(15.))
                            .line_height(px(20.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_color(rgb(tint)).child(name))
                            .child(div().text_color(rgb(0xc2c8d0)).child(arguments))
                            .children(result.map(|result| div().child(result)))
                            .into_any_element()
                    }
                }
            }));
        match self.scroll {
            Some(scroll) => list.track_scroll(&scroll),
            None => list,
        }
    }
}
