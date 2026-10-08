//! Recording identity and cursor checks for the native worker protocol.
use anyhow::{Result, ensure};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tag(u64, u64);
impl Tag {
    fn from(value: &Value) -> Option<Self> {
        Some(Self(
            value["session_id"].as_u64()?,
            value["generation"].as_u64()?,
        ))
    }
}
pub struct NativeSession {
    tag: Tag,
    cancelled: Option<Tag>,
    sent: u64,
    consumed: u64,
}
impl Default for NativeSession {
    fn default() -> Self {
        Self {
            tag: Tag(1, 1),
            cancelled: None,
            sent: 0,
            consumed: 0,
        }
    }
}
impl NativeSession {
    fn advance(&mut self) {
        self.tag.0 += 1;
        self.tag.1 += 1;
        self.sent = 0;
        self.consumed = 0;
    }
    pub fn command(&mut self, mut value: Value, samples: usize) -> Value {
        value["protocol_version"] = json!(crate::qwen_runtime::PROTOCOL);
        value["session_id"] = json!(self.tag.0);
        value["generation"] = json!(self.tag.1);
        self.sent += samples as u64;
        if value["type"] == "cancel" {
            self.cancelled = Some(self.tag);
            self.advance(); // Suppress queued results as soon as cancel is sent.
        }
        value
    }
    pub fn accept(&mut self, value: &Value) -> Result<bool> {
        ensure!(
            value["protocol_version"] == crate::qwen_runtime::PROTOCOL,
            "Unsupported QwenNative protocol version"
        );
        let kind = value["type"].as_str().unwrap_or("");
        if matches!(kind, "ready" | "error") {
            return Ok(true);
        }
        let tag = Tag::from(value);
        if kind == "status" && tag.is_none() {
            return Ok(true);
        }
        ensure!(
            tag.is_some(),
            "QwenNative response is missing session identity"
        );
        if kind == "cancelled" && tag == self.cancelled {
            self.cancelled = None;
            return Ok(true);
        }
        if tag != Some(self.tag) {
            return Ok(false);
        }
        if let Some(consumed) = value["consumed_samples"].as_u64() {
            ensure!(
                consumed >= self.consumed && consumed <= self.sent,
                "Invalid native recognition cursor: {consumed}, previous={}, sent={}",
                self.consumed,
                self.sent
            );
            self.consumed = consumed;
        }
        if kind == "final" {
            ensure!(
                value["consumed_samples"].as_u64() == Some(self.sent),
                "Native final does not cover all sent PCM"
            );
            self.advance();
        }
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn reply(kind: &str, id: u64, consumed: u64) -> Value {
        json!({"type":kind,"protocol_version":2,"session_id":id,"generation":id,"consumed_samples":consumed})
    }
    #[test]
    fn cancel_invalidates_already_queued_results_before_ack() {
        let mut session = NativeSession::default();
        session.command(json!({"type":"audio"}), 100);
        let cancel = session.command(json!({"type":"cancel"}), 0);
        assert_eq!(cancel["generation"], 1);
        assert!(!session.accept(&reply("final", 1, 100)).unwrap());
        assert!(session.accept(&reply("cancelled", 1, 0)).unwrap());
        assert_eq!(
            session.command(json!({"type":"audio"}), 20)["generation"],
            2
        );
        assert!(session.accept(&reply("final", 2, 20)).unwrap());
        assert!(!session.accept(&reply("partial", 2, 20)).unwrap());
    }
    #[test]
    fn final_requires_all_sent_pcm_and_cursor_cannot_go_backwards() {
        let mut session = NativeSession::default();
        session.command(json!({"type":"audio"}), 100);
        assert!(session.accept(&reply("partial", 1, 60)).unwrap());
        assert!(session.accept(&reply("partial", 1, 50)).is_err());
        assert!(session.accept(&reply("final", 1, 70)).is_err());
        assert!(session.accept(&reply("final", 1, 100)).unwrap());
    }
}
