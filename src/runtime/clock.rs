//! `Clock` injection boundary (Requirement 5.1): current-time retrieval
//! placed behind an abstract boundary, with a production implementation
//! backed by the system clock and a deterministic implementation
//! (Requirement 5.5) that always returns a fixed, constructed time so
//! tests can avoid depending on wall-clock time.

use time::OffsetDateTime;

/// Supplies the current time, decoupling callers from any concrete time
/// source (Requirement 5.1). Implementations must be safe to share across
/// threads (`Send + Sync`) since `RuntimeContext` hands out a single
/// shared instance to concurrent request handlers.
pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

/// Production `Clock` implementation backed by the system clock
/// (Requirement 5.6).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl SystemClock {
    pub fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Deterministic `Clock` implementation that always returns the fixed
/// time it was constructed with (Requirement 5.5), so tests can assert
/// against a known, reproducible time value instead of the flaky wall
/// clock.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    fixed: OffsetDateTime,
}

impl FixedClock {
    pub fn new(fixed: OffsetDateTime) -> Self {
        Self { fixed }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.fixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn fixed_clock_always_returns_the_same_constructed_time() {
        let fixed = datetime!(2026-07-04 12:00:00 UTC);
        let clock = FixedClock::new(fixed);

        assert_eq!(clock.now(), fixed);
        assert_eq!(clock.now(), fixed);
        assert_eq!(clock.now(), clock.now());
    }
}
