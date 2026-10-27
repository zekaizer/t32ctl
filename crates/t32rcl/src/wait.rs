// SPDX-License-Identifier: Apache-2.0
// Response matching ported from t32mcp src/t32api.rs `receive` (Apache-2.0, (c) 2026 Lauterbach
// GmbH) and pyrcl 1.1.6 _rc/hlinknet.py (MIT, (c) 2020 Lauterbach GmbH). See NOTICE.
//! Response matching and the deadline clock for one request. Sans-IO: every method takes `now`.

use std::time::{Duration, Instant};

use crate::{error::TimeoutKind, value::CallBudget};

/// `[0] == 0xFE` marks a keepalive. Status code 254 (`STD_ATTACH`) shares this byte; both
/// references treat it as keepalive too, so `Trace::keepalive_raw` keeps the bytes for review.
pub(crate) const T32_API_KEEPALIVE: u8 = 0xFE;
/// Ids are `counter % 255`: 1..=254, then 0. 255 is never sent (t32api.rs, hlinknet.py).
const ID_CYCLE: u16 = 255;
/// A response up to this many ids behind the outstanding request is stale, not a desync.
const STALE_WINDOW: u8 = 16;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Incoming {
    /// PowerView is alive and still working. Resets the idle clock.
    Keepalive,
    /// Empty or one-byte non-keepalive payload. Ignored, counted.
    Runt,
    /// Answer to a recent earlier request. Dropped, counted.
    Stale,
    Match,
    /// Neither the outstanding id nor a recent one: the stream is out of sync.
    Ahead {
        got: u8,
    },
}

pub(crate) fn classify(expected: u8, payload: &[u8]) -> Incoming {
    match payload {
        [] => Incoming::Runt,
        [T32_API_KEEPALIVE, ..] => Incoming::Keepalive,
        [_] => Incoming::Runt,
        [_, id, ..] if *id == expected => Incoming::Match,
        [_, id, ..] if is_recent_past(expected, *id) => Incoming::Stale,
        [_, id, ..] => Incoming::Ahead { got: *id },
    }
}

/// True when `got` was issued at most `STALE_WINDOW` requests before `expected`, modulo the id
/// cycle. t32api.rs uses `got < expected`, which turns a late 254 into a desync once ids wrap to 0.
pub(crate) fn is_recent_past(expected: u8, got: u8) -> bool {
    if u16::from(got) >= ID_CYCLE {
        return false;
    }
    let distance = (u16::from(expected) + ID_CYCLE - u16::from(got)) % ID_CYCLE;
    (1..=u16::from(STALE_WINDOW)).contains(&distance)
}

#[derive(Debug, Default)]
pub(crate) struct IdGen {
    counter: u32,
}

impl IdGen {
    pub fn next(&mut self) -> u8 {
        self.counter = self.counter.wrapping_add(1);
        (self.counter % u32::from(ID_CYCLE)) as u8
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Expired {
    pub kind: TimeoutKind,
    pub waited: Duration,
}

/// Idle and total clock for one outstanding request.
#[derive(Debug)]
pub(crate) struct Waiter {
    budget: CallBudget,
    started: Instant,
    last_activity: Instant,
    pub keepalives: u32,
}

impl Waiter {
    pub fn new(budget: CallBudget, now: Instant) -> Self {
        Self { budget, started: now, last_activity: now, keepalives: 0 }
    }

    /// Any inbound byte, a partial frame included, proves the peer is alive.
    pub fn on_activity(&mut self, now: Instant) {
        self.last_activity = now;
    }

    pub fn on_keepalive(&mut self, now: Instant) {
        self.keepalives += 1;
        self.on_activity(now);
    }

    /// Longest the next blocking read may wait, or the timeout that already happened.
    pub fn next_read_timeout(&self, now: Instant) -> Result<Duration, Expired> {
        let idle_used = now.saturating_duration_since(self.last_activity);
        if idle_used >= self.budget.idle {
            return Err(Expired { kind: TimeoutKind::Idle, waited: idle_used });
        }
        let mut left = self.budget.idle - idle_used;
        if let Some(total) = self.budget.total {
            let used = now.saturating_duration_since(self.started);
            if used >= total {
                return Err(Expired { kind: TimeoutKind::Total, waited: used });
            }
            left = left.min(total - used);
        }
        Ok(left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_run_1_to_254_then_0_and_never_255() {
        let mut ids = IdGen::default();
        let seq: Vec<u8> = (0..300).map(|_| ids.next()).collect();
        assert_eq!(seq[0], 1);
        assert_eq!(seq[253], 254);
        assert_eq!(seq[254], 0);
        assert_eq!(seq[255], 1);
        assert!(!seq.contains(&255));
    }

    #[test]
    fn classify_keeps_late_answers_apart_from_desync_across_the_wrap() {
        assert_eq!(classify(5, &[]), Incoming::Runt);
        assert_eq!(classify(5, &[0]), Incoming::Runt);
        assert_eq!(classify(5, &[0xFE]), Incoming::Keepalive);
        assert_eq!(classify(5, &[0xFE, 9]), Incoming::Keepalive);
        assert_eq!(classify(5, &[0, 5]), Incoming::Match);
        assert_eq!(classify(5, &[0, 4]), Incoming::Stale);
        assert_eq!(classify(0, &[0, 254]), Incoming::Stale);
        assert_eq!(classify(1, &[0, 250]), Incoming::Stale);
        assert_eq!(classify(0, &[0, 1]), Incoming::Ahead { got: 1 });
        assert_eq!(classify(5, &[0, 6]), Incoming::Ahead { got: 6 });
        assert_eq!(classify(5, &[0, 255]), Incoming::Ahead { got: 255 });
        assert_eq!(classify(100, &[0, 50]), Incoming::Ahead { got: 50 });
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn keepalive_resets_idle_but_never_total() {
        let t0 = Instant::now();
        let mut w = Waiter::new(CallBudget::idle(ms(300)).with_total(ms(1000)), t0);
        assert_eq!(w.next_read_timeout(t0), Ok(ms(300)));
        assert_eq!(w.next_read_timeout(t0 + ms(100)), Ok(ms(200)));
        for step in 1..=3 {
            w.on_keepalive(t0 + ms(250 * step));
        }
        assert_eq!(w.next_read_timeout(t0 + ms(800)), Ok(ms(200)));
        assert_eq!(w.next_read_timeout(t0 + ms(1000)), Err(Expired { kind: TimeoutKind::Total, waited: ms(1000) }));
        assert_eq!(w.keepalives, 3);
    }

    #[test]
    fn silence_past_idle_expires_and_partial_bytes_count_as_activity() {
        let t0 = Instant::now();
        let mut w = Waiter::new(CallBudget::idle(ms(300)), t0);
        w.on_activity(t0 + ms(200));
        assert_eq!(w.next_read_timeout(t0 + ms(400)), Ok(ms(100)));
        assert_eq!(w.next_read_timeout(t0 + ms(500)), Err(Expired { kind: TimeoutKind::Idle, waited: ms(300) }));
    }
}
