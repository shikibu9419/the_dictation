use gpui::{App, ClickEvent, ElementId, SharedString, Window, div, prelude::*, px, rgb};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Selectable pill used in option groups.
#[derive(IntoElement)]
pub struct Choice {
    id: ElementId,
    label: SharedString,
    selected: bool,
    on_click: Option<ClickHandler>,
}
impl Choice {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected,
            on_click: None,
        }
    }
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}
impl RenderOnce for Choice {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let pill = div()
            .id(self.id)
            .flex_1()
            .px(px(14.))
            .py(px(12.))
            .rounded(px(10.))
            .cursor_pointer()
            .bg(if self.selected {
                rgb(0x484848)
            } else {
                rgb(0x262626)
            })
            .text_color(if self.selected {
                rgb(0xf4f4f4)
            } else {
                rgb(0xa2a2a2)
            })
            .child(self.label);
        match self.on_click {
            Some(handler) => pill.on_click(move |event, window, cx| handler(event, window, cx)),
            None => pill,
        }
    }
}
