use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Copy this policy into each new candidate. Updating defaults cannot move the
/// deadlines of an already observed operation.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Reception {
    pub hold_ui_delay_ms: u64,
    pub long_resume_grace_ms: u64,
    pub tap_sequence_grace_ms: u64,
    pub live_chunk_ms: u64,
    pub state_poll_interval_ms: u64,
}
impl Default for Reception {
    fn default() -> Self {
        Self {
            hold_ui_delay_ms: 50,
            long_resume_grace_ms: 50,
            tap_sequence_grace_ms: 50,
            live_chunk_ms: 200,
            state_poll_interval_ms: 50,
        }
    }
}
impl Reception {
    pub fn validate(self) -> Result<()> {
        for (name, value) in [
            ("hold_ui_delay_ms", self.hold_ui_delay_ms),
            ("long_resume_grace_ms", self.long_resume_grace_ms),
            ("tap_sequence_grace_ms", self.tap_sequence_grace_ms),
        ] {
            ensure!(value <= 5000, "{name} must be between 0 and 5000");
        }
        ensure!(
            (1..=10000).contains(&self.live_chunk_ms),
            "live_chunk_ms must be between 1 and 10000"
        );
        ensure!(
            (1..=60000).contains(&self.state_poll_interval_ms),
            "state_poll_interval_ms must be between 1 and 60000"
        );
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_fields_use_independent_defaults_and_unknown_fields_fail() {
        let policy: Reception = serde_json::from_str(r#"{"hold_ui_delay_ms":30}"#).unwrap();
        assert_eq!(policy.hold_ui_delay_ms, 30);
        assert_eq!(policy.long_resume_grace_ms, 50);
        assert_eq!(policy.tap_sequence_grace_ms, 50);
        assert_eq!(policy.live_chunk_ms, 200);
        assert_eq!(policy.state_poll_interval_ms, 50);
        assert!(serde_json::from_str::<Reception>(r#"{"hold_ui_delai_ms":30}"#).is_err());
    }
    #[test]
    fn snapshots_do_not_change_when_the_next_candidate_policy_changes() {
        let mut defaults = Reception::default();
        let captured = defaults;
        defaults.hold_ui_delay_ms = 20;
        defaults.long_resume_grace_ms = 100;
        assert_eq!(captured, Reception::default());
        assert_ne!(captured, defaults);
    }
    #[test]
    fn zero_grace_is_valid_but_polling_and_audio_batches_must_make_progress() {
        let mut policy = Reception {
            hold_ui_delay_ms: 0,
            long_resume_grace_ms: 0,
            tap_sequence_grace_ms: 0,
            ..Reception::default()
        };
        policy.validate().unwrap();
        policy.state_poll_interval_ms = 0;
        assert!(policy.validate().is_err());
        policy.state_poll_interval_ms = 50;
        policy.live_chunk_ms = 0;
        assert!(policy.validate().is_err());
        policy.live_chunk_ms = 200;
        policy.hold_ui_delay_ms = u64::MAX;
        assert!(policy.validate().is_err());
    }
}
