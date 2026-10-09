//! Structured diagnostics for sampled recording-state edges.
//! These observations are not firmware-provided physical switch timestamps.
use pebble_core::output::Output;
use serde_json::json;
use std::time::Instant;

struct Press {
    id: u64,
    at: Instant,
    wall: String,
    down_sample_gap_ms: Option<f64>,
}
#[derive(Default)]
pub struct ButtonTiming {
    sequence: u64,
    previous: Option<(bool, Instant)>,
    active: Option<Press>,
}
fn milliseconds(start: Instant, end: Instant) -> f64 {
    end.duration_since(start).as_secs_f64() * 1000.
}
impl ButtonTiming {
    pub fn observe(&mut self, pressed: bool, output: &Output) {
        let now = Instant::now();
        let wall = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let previous = self.previous.replace((pressed, now));
        if previous.is_some_and(|(value, _)| value == pressed) {
            return;
        }
        if pressed {
            self.sequence += 1;
            let press = Press {
                id: self.sequence,
                at: now,
                wall,
                down_sample_gap_ms: previous.map(|(_, at)| milliseconds(at, now)),
            };
            output.debug(format!("button_timing {}", json!({
                "event":"down", "press_id":press.id, "pid":std::process::id(),
                "observed_down_at":press.wall, "source":"sampled_in_collection_state",
                "down_sample_gap_ms":press.down_sample_gap_ms,
                "status":if previous.is_some() {"observed"} else {"already_down_at_first_sample"}
            })));
            self.active = Some(press);
        } else if let Some(press) = self.active.take() {
            output.debug(format!("button_timing {}", json!({
                "event":"press_duration", "press_id":press.id, "pid":std::process::id(),
                "observed_down_at":press.wall, "observed_up_at":wall,
                "observed_hold_ms":milliseconds(press.at, now),
                "physical_hold_ms":null,
                "down_sample_gap_ms":press.down_sample_gap_ms,
                "up_sample_gap_ms":previous.map(|(_, at)| milliseconds(at, now)),
                "source":"sampled_in_collection_state",
                "status":if press.down_sample_gap_ms.is_some() {"sampled_edges"} else {"start_unknown"}
            })));
        }
    }
    pub fn disconnect(&mut self, output: &Output) {
        if let Some(press) = self.active.take() {
            output.debug(format!(
                "button_timing {}",
                json!({
                    "event":"press_duration", "press_id":press.id, "pid":std::process::id(),
                    "observed_down_at":press.wall, "observed_up_at":null,
                    "observed_hold_ms":null, "physical_hold_ms":null,
                    "status":"interrupted_before_up", "source":"sampled_in_collection_state"
                })
            ));
        }
        self.previous = None;
    }
}
