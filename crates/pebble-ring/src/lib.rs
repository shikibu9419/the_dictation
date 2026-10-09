//! Index 01 ring support: BLE reception, collection decoding, the gesture
//! reducer and the PCM input boundary consumed by applications.
pub mod bluetooth;
pub mod button_timing;
pub mod capture;
pub mod collection;
pub mod input;
pub mod pairing;
pub mod reception;
pub mod recordings;
#[cfg(test)]
mod tests;
