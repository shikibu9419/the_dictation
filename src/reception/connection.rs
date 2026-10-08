//! Connection lifetime is independent of recording and recognition state.
const IDLE_MS: u64 = 3_600_000;
const FAILURE_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchReason {
    Idle,
    Failure,
}

#[derive(Default, Debug)]
pub struct ConnectionPolicy {
    last_activity: u64,
    failed_since: Option<u64>,
    failures: u32,
    watch: Option<WatchReason>,
}

impl ConnectionPolicy {
    pub fn activity(&mut self, now: u64) {
        self.last_activity = now;
    }

    /// Opening a CoreBluetooth connection is not recovery. Call only after
    /// S/R agree there is no pending data or after C actually transfers.
    pub fn progress(&mut self, now: u64) -> Option<u64> {
        self.watch = None;
        self.failures = 0;
        self.failed_since
            .take()
            .map(|since| now.saturating_sub(since))
    }

    pub fn failed(&mut self, now: u64) {
        self.failed_since.get_or_insert(now);
        self.failures = self.failures.saturating_add(1);
    }

    pub fn mode(&mut self, now: u64) -> Option<WatchReason> {
        if self
            .failed_since
            .is_some_and(|since| now.saturating_sub(since) >= FAILURE_MS)
        {
            self.watch = Some(WatchReason::Failure);
        }
        self.watch
    }

    pub fn idle(&mut self, now: u64, collecting: bool, caught_up: bool) -> bool {
        if !collecting && caught_up && now.saturating_sub(self.last_activity) >= IDLE_MS {
            self.watch = Some(WatchReason::Idle);
            true
        } else {
            false
        }
    }

    /// A fresh hint gives the returning user a new connected-use interval.
    /// It does not clear a persistent failure window until real data progresses.
    pub fn resume_hint(&mut self, now: u64) {
        self.activity(now);
        if self.watch == Some(WatchReason::Idle) {
            self.watch = None;
        }
    }

    pub fn retry_delay_ms(&self) -> u64 {
        250u64
            .saturating_mul(1 << self.failures.saturating_sub(1).min(5))
            .min(5_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connected_idle_only_expires_after_one_hour_and_not_during_capture_or_backlog() {
        let mut p = ConnectionPolicy::default();
        assert!(!p.idle(30_000, false, true));
        assert!(!p.idle(IDLE_MS - 1, false, true));
        assert!(!p.idle(IDLE_MS, true, true));
        assert!(!p.idle(IDLE_MS, false, false));
        assert!(p.idle(IDLE_MS, false, true));
        assert_eq!(p.mode(IDLE_MS), Some(WatchReason::Idle));
        p.resume_hint(IDLE_MS + 1);
        assert_eq!(p.mode(IDLE_MS + 1), None);
        p.progress(IDLE_MS + 2);
        assert!(!p.idle(IDLE_MS + 3, false, true));
    }

    #[test]
    fn repeated_connect_failures_share_one_minute_window() {
        let mut p = ConnectionPolicy::default();
        p.failed(5_000);
        for now in (10_000..65_000).step_by(5_000) {
            p.failed(now);
            assert_eq!(p.mode(now), None);
        }
        assert_eq!(p.mode(65_000), Some(WatchReason::Failure));
        p.resume_hint(65_001);
        p.failed(65_100);
        assert_eq!(p.mode(65_100), Some(WatchReason::Failure));
        assert_eq!(p.progress(66_000), Some(61_000));
        assert_eq!(p.mode(66_000), None);
        p.failed(70_000);
        assert_eq!(p.mode(70_000), None);
    }

    #[test]
    fn repeated_success_does_not_reset_idle_and_backoff_is_bounded() {
        let mut p = ConnectionPolicy::default();
        p.activity(1_000);
        for now in 1_000..1_100 {
            p.progress(now);
        }
        assert!(p.idle(IDLE_MS + 1_000, false, true));
        for now in 0..40 {
            p.failed(now);
            assert!((250..=5_000).contains(&p.retry_delay_ms()));
        }
        assert_eq!(p.retry_delay_ms(), 5_000);
    }
}
