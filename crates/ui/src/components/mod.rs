//! Presentation-only components. Each takes its state as props and reports
//! interaction through callbacks; none reads application state.
mod choice;
mod conversation;
mod mic_icon;
mod panel_surface;
mod recording_dot;
mod spinner;
mod transcript;

pub use choice::Choice;
pub use conversation::{Conversation, ToolStatus, Turn};
pub use mic_icon::MicIcon;
pub use panel_surface::PanelSurface;
pub use recording_dot::RecordingDot;
pub use spinner::Spinner;
pub use transcript::Transcript;

pub(crate) fn pulse(delta: f32) -> f32 {
    (1. - (delta * std::f32::consts::TAU).cos()) / 2.
}
