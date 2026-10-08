//! Serial Telesto request selection. All times are monotonic milliseconds.
//! The caller completes a READ before asking for another one: the wire protocol
//! has no request IDs with which to multiplex responses.
use anyhow::{Context, Result, ensure};

const RANGE_INTERVAL_MS: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    State,
    Range,
    Collection(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Read {
        request: Request,
        deadline_lateness_ms: u64,
    },
    WaitUntil(u64),
}

#[derive(Debug)]
pub struct Scheduler {
    period_ms: u64,
    state_due: u64,
    range_due: u64,
    end: u16,
    cursor: u16,
    range_needed: bool,
    state_since_work: bool,
    last_work: Option<Request>,
    in_flight: Option<(Request, u64)>,
}

impl Scheduler {
    /// The initial S/R and startup boundary have already been read. A reconnect
    /// seeds the cursor from retained receiver state, never from an ASR cursor.
    pub fn new(
        period_ms: u64,
        state_started: u64,
        range_started: u64,
        start: u16,
        end: u16,
        cursor: u16,
        observed_count: u8,
    ) -> Result<Self> {
        ensure!(period_ms > 0, "State polling period must be positive");
        ensure!(end.wrapping_sub(start) <= 512, "Invalid collection range");
        Ok(Self {
            period_ms,
            state_due: state_started.saturating_add(period_ms),
            range_due: range_started.saturating_add(RANGE_INTERVAL_MS),
            end,
            cursor: clamp_cursor(cursor, start, end),
            range_needed: observed_count != end as u8,
            state_since_work: false,
            last_work: None,
            in_flight: None,
        })
    }

    pub fn cursor(&self) -> u16 {
        self.cursor
    }

    pub fn caught_up(&self) -> bool {
        self.cursor == self.end && !self.range_needed && self.in_flight.is_none()
    }

    /// Start-to-start cadence; missed periods are not replayed in a burst.
    /// If an S response itself exceeds the period, give one queued R/C a turn
    /// before the next S. Conversely, slow periodic R must not starve C.
    pub fn next(&mut self, now: u64) -> Result<Decision> {
        ensure!(
            self.in_flight.is_none(),
            "A Telesto READ is already in flight"
        );
        let backlog = self.cursor != self.end;
        let periodic_range = now >= self.range_due;
        let work = if (!backlog && self.range_needed)
            || (periodic_range && (!backlog || self.last_work != Some(Request::Range)))
        {
            Some(Request::Range)
        } else if backlog {
            Some(Request::Collection(self.cursor))
        } else {
            None
        };
        let request = if now >= self.state_due && (!self.state_since_work || work.is_none()) {
            Request::State
        } else if let Some(work) = work {
            work
        } else {
            return Ok(Decision::WaitUntil(self.state_due.min(self.range_due)));
        };
        let deadline_lateness_ms = match request {
            Request::State => now.saturating_sub(self.state_due),
            Request::Range if periodic_range => now.saturating_sub(self.range_due),
            _ => 0,
        };
        self.in_flight = Some((request, now));
        Ok(Decision::Read {
            request,
            deadline_lateness_ms,
        })
    }

    fn finish(&mut self, request: Request) -> Result<u64> {
        let (pending, started) = self.in_flight.context("No Telesto READ in flight")?;
        ensure!(pending == request, "Mismatched Telesto READ completion");
        self.in_flight = None;
        Ok(started)
    }

    /// Returns whether the low-byte count proves that R needs refreshing. It is
    /// only a hint, not an invented 16-bit end position (wrap/reset is ambiguous).
    pub fn state(&mut self, count: u8, finished: u64) -> Result<bool> {
        let started = self.finish(Request::State)?;
        self.state_due = started.saturating_add(self.period_ms);
        self.state_since_work = finished >= self.state_due;
        self.range_needed |= count != self.end as u8;
        Ok(count != self.end as u8)
    }

    pub fn range(&mut self, start: u16, end: u16) -> Result<()> {
        ensure!(end.wrapping_sub(start) <= 512, "Invalid collection range");
        let started = self.finish(Request::Range)?;
        self.range_due = started.saturating_add(RANGE_INTERVAL_MS);
        self.end = end;
        self.cursor = clamp_cursor(self.cursor, start, end);
        self.range_needed = false;
        self.state_since_work = false;
        self.last_work = Some(Request::Range);
        Ok(())
    }

    pub fn collection(&mut self, index: u16) -> Result<()> {
        self.finish(Request::Collection(index))?;
        self.cursor = index.wrapping_add(1);
        self.range_needed |= self.cursor == self.end;
        self.state_since_work = false;
        self.last_work = Some(Request::Collection(index));
        Ok(())
    }
}

fn clamp_cursor(cursor: u16, start: u16, end: u16) -> u16 {
    if cursor.wrapping_sub(start) <= end.wrapping_sub(start) {
        cursor
    } else {
        start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(s: &mut Scheduler, now: u64) -> Request {
        match s.next(now).unwrap() {
            Decision::Read { request, .. } => request,
            decision => panic!("Expected READ, got {decision:?}"),
        }
    }

    #[test]
    fn idle_is_start_to_start_and_does_not_read_range_on_every_state() {
        let mut s = Scheduler::new(50, 0, 0, 1, 1, 1, 1).unwrap();
        assert_eq!(s.next(10).unwrap(), Decision::WaitUntil(50));
        for time in (50..=1_000).step_by(50) {
            assert_eq!(read(&mut s, time), Request::State);
            assert!(!s.state(1, time + 10).unwrap());
        }
        assert_eq!(read(&mut s, 1_010), Request::Range);
        s.range(1, 1).unwrap();
        assert_eq!(s.next(1_020).unwrap(), Decision::WaitUntil(1_050));
    }

    #[test]
    fn backlog_has_no_sleep_or_range_bundle_per_chunk() {
        let mut s = Scheduler::new(50, 0, 0, 1, 4, 1, 4).unwrap();
        for i in 1..4 {
            assert_eq!(read(&mut s, i as u64 * 5), Request::Collection(i));
            s.collection(i).unwrap();
        }
        assert!(!s.caught_up()); // One refresh after draining the known range.
        assert_eq!(read(&mut s, 20), Request::Range);
        s.range(1, 4).unwrap();
        assert!(s.caught_up());
        assert_eq!(s.next(25).unwrap(), Decision::WaitUntil(50));
    }

    #[test]
    fn state_deadline_interrupts_backlog_after_one_atomic_read() {
        let mut s = Scheduler::new(50, 0, 0, 1, 4, 1, 4).unwrap();
        assert_eq!(read(&mut s, 10), Request::Collection(1));
        assert!(s.next(50).is_err()); // No concurrent read of the same wire.
        s.collection(1).unwrap();
        assert_eq!(
            s.next(310).unwrap(),
            Decision::Read {
                request: Request::State,
                deadline_lateness_ms: 260,
            }
        );
        s.state(5, 320).unwrap();
        assert_eq!(read(&mut s, 320), Request::Collection(2));
    }

    #[test]
    fn slow_state_and_range_never_starve_audio_or_catch_up_in_bursts() {
        let mut s = Scheduler::new(50, 0, 0, 1, 4, 1, 4).unwrap();
        assert_eq!(read(&mut s, 50), Request::State);
        s.state(4, 1_100).unwrap();
        assert_eq!(read(&mut s, 1_100), Request::Range);
        s.range(1, 4).unwrap();
        assert_eq!(read(&mut s, 2_200), Request::State);
        s.state(4, 3_300).unwrap();
        assert_eq!(read(&mut s, 3_300), Request::Collection(1));
        s.collection(1).unwrap();
        assert_eq!(read(&mut s, 3_310), Request::State);
        s.state(4, 3_320).unwrap();
        assert_eq!(read(&mut s, 3_320), Request::Range);
    }

    #[test]
    fn count_change_refreshes_range_without_waiting_for_period() {
        let mut s = Scheduler::new(50, 0, 0, 1, 1, 1, 1).unwrap();
        assert_eq!(read(&mut s, 50), Request::State);
        assert!(s.state(2, 60).unwrap());
        assert_eq!(read(&mut s, 60), Request::Range);
        s.range(1, 2).unwrap();
        assert_eq!(read(&mut s, 70), Request::Collection(1));
    }

    #[test]
    fn rollover_eviction_and_invalid_completion_are_explicit() {
        let mut s = Scheduler::new(50, 0, 0, 65_534, 1, 65_535, 1).unwrap();
        assert_eq!(read(&mut s, 1), Request::Collection(65_535));
        assert!(s.collection(0).is_err());
        assert!(s.next(2).is_err()); // A wrong completion didn't clear the request.
        s.collection(65_535).unwrap();
        assert_eq!(read(&mut s, 3), Request::Collection(0));
        s.collection(0).unwrap();
        assert_eq!(read(&mut s, 4), Request::Range);
        s.range(3, 5).unwrap(); // Missing source is reported by the range consumer.
        assert_eq!(s.cursor(), 3);
        assert_eq!(read(&mut s, 5), Request::Collection(3));
    }

    #[test]
    fn identical_low_byte_still_gets_periodic_full_range() {
        let mut s = Scheduler::new(5_000, 0, 0, 1, 1, 1, 1).unwrap();
        assert_eq!(read(&mut s, 1_000), Request::Range);
        s.range(1, 257).unwrap();
        assert_eq!(read(&mut s, 1_010), Request::Collection(1));
    }
}
