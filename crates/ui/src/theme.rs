//! Shared dark, transparent theme for floating panels.
use gpui::{App, rgb, rgba, transparent_black};
use gpui_component::{Theme, ThemeMode};

pub const TEXT: u32 = 0xf3f6fa;
pub const ACCENT: u32 = 0xff334b;
pub const FONT: &str = ".AppleSystemUIFont";

pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::global_mut(cx).background = transparent_black();
    Theme::global_mut(cx).selection = rgba(0x62ddff40).into();
    Theme::global_mut(cx).caret = rgb(0x8bedff).into();
}
