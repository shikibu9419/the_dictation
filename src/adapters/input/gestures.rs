//! Device-independent tap grouping and action hooks. Receipt times are used;
//! callers must reset at disconnect/startup and must not replay buffered input.
use crate::output::Output;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    SingleTap,
    DoubleTap,
}
#[derive(Clone, Copy, Debug)]
pub struct GestureEvent {
    pub gesture: Gesture,
    pub first_collection: u16,
    pub last_collection: u16,
}
#[derive(Clone, Copy, Debug)]
pub enum Press {
    Short,
    Hold,
}
struct Pending {
    count: usize,
    first: u16,
    last: u16,
    at: Instant,
}
pub struct Detector {
    window: Duration,
    last_seen: Option<u16>,
    pending: Option<Pending>,
}
impl Detector {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last_seen: None,
            pending: None,
        }
    }
    pub fn reset(&mut self) {
        self.last_seen = None;
        self.pending = None;
    }
    pub fn poll(&mut self, now: Instant) -> Option<GestureEvent> {
        if self
            .pending
            .as_ref()
            .is_none_or(|p| now.duration_since(p.at) < self.window)
        {
            return None;
        }
        let p = self.pending.take().unwrap();
        let gesture = match p.count {
            1 => Gesture::SingleTap,
            2 => Gesture::DoubleTap,
            _ => return None,
        };
        Some(GestureEvent {
            gesture,
            first_collection: p.first,
            last_collection: p.last,
        })
    }
    pub fn observe(&mut self, index: u16, press: Press, now: Instant) -> Option<GestureEvent> {
        // Collection indices wrap at 65536. Ignore duplicates/replayed packets.
        if self
            .last_seen
            .is_some_and(|last| index.wrapping_sub(last) == 0 || index.wrapping_sub(last) >= 32768)
        {
            return None;
        }
        self.last_seen = Some(index);
        let completed = self.poll(now);
        match press {
            Press::Hold => self.pending = None,
            Press::Short => match &mut self.pending {
                Some(p) => {
                    p.count += 1;
                    p.last = index;
                    p.at = now;
                }
                None => {
                    self.pending = Some(Pending {
                        count: 1,
                        first: index,
                        last: index,
                        at: now,
                    })
                }
            },
        }
        completed
    }
}

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
            "Gesture {:?}: collections={}..{} (receiver timing)",
            event.gesture, event.first_collection, event.last_collection
        ));
    }
}
