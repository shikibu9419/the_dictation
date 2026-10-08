//! Capture MLX errors without its default handler writing to protocol stdout.
use std::{
    cell::RefCell,
    ffi::{c_char, c_void, CStr},
};
thread_local! { static LAST: RefCell<Option<String>> = const { RefCell::new(None) }; }
unsafe extern "C" fn record(message: *const c_char, _: *mut c_void) {
    // The callback must never unwind across the C ABI.
    let _ = std::panic::catch_unwind(|| {
        let text = if message.is_null() {
            "Unknown MLX error".to_owned()
        } else {
            CStr::from_ptr(message).to_string_lossy().into_owned()
        };
        LAST.with(|last| *last.borrow_mut() = Some(text));
    });
}
pub fn install() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        super::ffi::mlx_set_error_handler(record, std::ptr::null_mut(), None);
    });
}
#[track_caller]
pub fn check(status: i32, operation: &str) {
    if status != 0 {
        let message = LAST
            .with(|last| last.borrow_mut().take())
            .unwrap_or_default();
        panic!("{operation} failed ({status}): {message}");
    }
}
