//! Transport-independent gesture values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gesture {
    SingleTap,
    DoubleTap,
}
#[derive(Clone, Copy, Debug)]
pub struct GestureEvent {
    pub gesture: Gesture,
    pub first_collection: Option<u16>,
    pub last_collection: Option<u16>,
}
