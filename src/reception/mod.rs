//! Device-independent reception policy. Observation normalization and state
//! transitions have no PCM, Bluetooth, speech-engine, or window dependencies.
pub mod button_detector;
pub mod config;
pub mod connection;
pub mod input_effects;
pub mod scheduler;
pub mod session_state;
#[cfg(test)]
mod session_state_tests;
