//! Gesture action hooks. Detection and state policy live in separate modules.
pub use super::gesture_types::GestureEvent;
use crate::output::Output;
/// Hooks run on the input worker. Queue expensive work elsewhere; never block BLE.
pub trait GestureHook: Send {
    fn on_gesture(&mut self, event: GestureEvent, output: &Output);
}
#[derive(Default)]
pub struct Hooks {
    handlers: Vec<Box<dyn GestureHook>>,
}
impl Hooks {
    pub fn register(&mut self, hook: impl GestureHook + 'static) {
        self.handlers.push(Box::new(hook));
    }
    pub fn dispatch(&mut self, event: GestureEvent, output: &Output) {
        for handler in &mut self.handlers {
            handler.on_gesture(event, output);
        }
    }
}
pub struct LogHook;
impl GestureHook for LogHook {
    fn on_gesture(&mut self, event: GestureEvent, output: &Output) {
        output.debug(format!(
            "Gesture {:?}: collections={:?}..{:?} (receiver timing)",
            event.gesture, event.first_collection, event.last_collection
        ));
    }
}
