//! Device-independent tap grouping and action hooks. Receipt times are used;
//! callers must reset at disconnect/startup and must not replay buffered input.
use crate::output::Output;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_double_and_hold_are_exclusive() {
        let now = Instant::now();
        let mut d = Detector::new(Duration::from_millis(500));
        assert!(d.observe(10, Press::Short, now).is_none());
        assert!(d.poll(now + Duration::from_millis(499)).is_none());
        assert_eq!(
            d.poll(now + Duration::from_millis(500)).unwrap().gesture,
            Gesture::SingleTap
        );
        assert!(d.poll(now + Duration::from_secs(2)).is_none());
        d.observe(11, Press::Short, now + Duration::from_secs(2));
        d.observe(12, Press::Short, now + Duration::from_millis(2200));
        assert_eq!(
            d.poll(now + Duration::from_millis(2700)).unwrap().gesture,
            Gesture::DoubleTap
        );
        d.observe(13, Press::Short, now + Duration::from_secs(3));
        d.observe(14, Press::Hold, now + Duration::from_millis(3200));
        assert!(d.poll(now + Duration::from_secs(4)).is_none());
    }
    #[test]
    fn replay_disconnect_and_triple_taps_do_not_trigger_actions() {
        let now = Instant::now();
        let mut d = Detector::new(Duration::from_millis(500));
        d.observe(65535, Press::Short, now);
        d.observe(65535, Press::Short, now);
        d.observe(0, Press::Short, now);
        d.observe(1, Press::Short, now);
        assert!(d.poll(now + Duration::from_secs(1)).is_none());
        d.observe(2, Press::Short, now + Duration::from_secs(2));
        d.reset();
        assert!(d.poll(now + Duration::from_secs(3)).is_none());
    }
}
