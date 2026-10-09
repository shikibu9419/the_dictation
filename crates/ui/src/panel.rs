//! Safe wrapper over `native/OverlayMac.m`: the floating NSPanel, its audio
//! glow, status menu and IME hooks. One panel exists per process.
use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ffi::{CString, c_char, c_void};

mod ffi {
    use std::ffi::{c_char, c_void};
    unsafe extern "C" {
        pub fn index_panel_setup(
            view: *mut c_void,
            callback: extern "C" fn(i32),
            titles: *const *const c_char,
            tags: *const i32,
            keys: *const *const c_char,
            count: i32,
        );
        pub fn index_frontmost_pid() -> i32;
        pub fn index_panel_show(target: i32);
        pub fn index_panel_request_frame();
        pub fn index_panel_editing(editing: bool);
        pub fn index_panel_hide();
        pub fn index_panel_audio_state(recording: u64);
        pub fn index_panel_audio_level(recording: u64, level: f64);
        pub fn index_panel_resize(width: f64, height: f64, circular: bool);
        pub fn index_reduce_motion() -> bool;
        pub fn index_panel_visible() -> bool;
        pub fn index_status(text: *const c_char);
        pub fn index_permission();
    }
}

/// Status-bar menu entry. The first entry is the status line and gets no action.
#[derive(Clone, Copy, Debug)]
pub struct MenuItem {
    pub title: &'static str,
    /// Delivered to the setup callback when the item is chosen.
    pub tag: i32,
    /// Key equivalent such as `","`; empty for none.
    pub key: &'static str,
}

/// Attach the floating-panel chrome to the window and install the status menu.
pub fn setup(window: &Window, menu: &[MenuItem], on_menu: extern "C" fn(i32)) {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)
        .expect("window handle")
        .as_raw()
    else {
        panic!("floating panel requires an AppKit window");
    };
    let titles: Vec<CString> = menu
        .iter()
        .map(|item| CString::new(item.title).expect("menu title"))
        .collect();
    let keys: Vec<CString> = menu
        .iter()
        .map(|item| CString::new(item.key).expect("menu key"))
        .collect();
    let title_ptrs: Vec<*const c_char> = titles.iter().map(|t| t.as_ptr()).collect();
    let key_ptrs: Vec<*const c_char> = keys.iter().map(|k| k.as_ptr()).collect();
    let tags: Vec<i32> = menu.iter().map(|item| item.tag).collect();
    unsafe {
        ffi::index_panel_setup(
            handle.ns_view.as_ptr() as *mut c_void,
            on_menu,
            title_ptrs.as_ptr(),
            tags.as_ptr(),
            key_ptrs.as_ptr(),
            menu.len() as i32,
        );
    }
}
pub fn frontmost_pid() -> i32 {
    unsafe { ffi::index_frontmost_pid() }
}
pub fn show(target_pid: i32) {
    unsafe { ffi::index_panel_show(target_pid) }
}
pub fn request_frame() {
    unsafe { ffi::index_panel_request_frame() }
}
pub fn editing(editing: bool) {
    unsafe { ffi::index_panel_editing(editing) }
}
pub fn hide() {
    unsafe { ffi::index_panel_hide() }
}
/// Start (non-zero id) or stop (0) the audio glow for a recording.
pub fn audio_state(recording: u64) {
    unsafe { ffi::index_panel_audio_state(recording) }
}
pub fn audio_level(recording: u64, level: f64) {
    unsafe { ffi::index_panel_audio_level(recording, level) }
}
pub fn resize(width: f64, height: f64, circular: bool) {
    unsafe { ffi::index_panel_resize(width, height, circular) }
}
pub fn reduce_motion() -> bool {
    unsafe { ffi::index_reduce_motion() }
}
pub fn visible() -> bool {
    unsafe { ffi::index_panel_visible() }
}
/// Replace the status line (first menu item).
pub fn status(text: &str) {
    if let Ok(text) = CString::new(text) {
        unsafe { ffi::index_status(text.as_ptr()) }
    }
}
pub fn request_permission() {
    unsafe { ffi::index_permission() }
}
