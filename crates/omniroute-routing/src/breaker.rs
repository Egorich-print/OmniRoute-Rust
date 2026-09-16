//! Circuit breakers with explicit time.
//!
//! Mirrors the JS breaker registry semantics (`CLOSED` serving, `OPEN`
//! shedding after `threshold` consecutive failures, half-open trial after
//! `cooldown`). Time is injected (`std::time::Instant` parameters) so tests
//! never sleep.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Serving normally.
    Closed,
    /// Shedding load until the cooldown elapses.
    Open,
}

/// Consecutive-failure breaker for one backend.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    threshold: u32,
    cooldown: Duration,
    failures: u32,
    opened_at: Option<Instant>,
}

impl CircuitBreaker {
    /// Build a breaker that opens after `threshold` consecutive failures and
    /// allows a trial after `cooldown_secs`.
    pub fn new(threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            threshold: threshold.max(1),
            cooldown: Duration::from_secs(cooldown_secs),
            failures: 0,
            opened_at: None,
        }
    }

    /// Raw state (ignores cooldown expiry; see [`CircuitBreaker::allow`]).
    pub fn state(&self) -> BreakerState {
        if self.opened_at.is_some() {
            BreakerState::Open
        } else {
            BreakerState::Closed
        }
    }

    /// Whether a request may proceed now. An open breaker whose cooldown has
    /// elapsed admits one trial (it stays open until that trial succeeds).
    pub fn allow(&self, now: Instant) -> bool {
        match self.opened_at {
            None => true,
            Some(opened) => now.duration_since(opened) >= self.cooldown,
        }
    }

    /// Record a success: close and reset.
    pub fn record_success(&mut self) {
        self.failures = 0;
        self.opened_at = None;
    }

    /// Record a failure: open once the threshold is reached (restarting the
    /// cooldown on every failure while open).
    pub fn record_failure(&mut self, now: Instant) {
        self.failures += 1;
        if self.failures >= self.threshold {
            self.opened_at = Some(now);
        }
    }
}

/// Named breakers, one per backend.
#[derive(Debug, Default)]
pub struct BreakerSet {
    breakers: HashMap<String, CircuitBreaker>,
    threshold: u32,
    cooldown_secs: u64,
}

impl BreakerSet {
    /// Empty set; breakers are created lazily with these settings.
    pub fn new(threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            breakers: HashMap::new(),
            threshold,
            cooldown_secs,
        }
    }

    fn breaker(&mut self, backend: &str) -> &mut CircuitBreaker {
        self.breakers
            .entry(backend.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.threshold, self.cooldown_secs))
    }

    /// Whether `backend` may serve now.
    pub fn allow(&mut self, backend: &str, now: Instant) -> bool {
        self.breaker(backend).allow(now)
    }

    /// Record a success for `backend`.
    pub fn record_success(&mut self, backend: &str) {
        self.breaker(backend).record_success();
    }

    /// Record a failure for `backend`.
    pub fn record_failure(&mut self, backend: &str, now: Instant) {
        self.breaker(backend).record_failure(now);
    }

    /// Raw state for inspection.
    pub fn state(&mut self, backend: &str) -> BreakerState {
        self.breaker(backend).state()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_after_threshold_and_recovers() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(3, 60);

        assert!(breaker.allow(start));
        breaker.record_failure(start);
        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert!(breaker.allow(start));

        breaker.record_failure(start);
        assert_eq!(breaker.state(), BreakerState::Open);
        assert!(!breaker.allow(start));

        // Cooldown elapsed: trial admitted, still open until it succeeds.
        let later = start + Duration::from_secs(61);
        assert!(breaker.allow(later));
        assert_eq!(breaker.state(), BreakerState::Open);

        breaker.record_success();
        assert_eq!(breaker.state(), BreakerState::Closed);
        assert!(breaker.allow(later));
    }

    #[test]
    fn failure_while_open_restarts_cooldown() {
        let start = Instant::now();
        let mut breaker = CircuitBreaker::new(1, 60);
        breaker.record_failure(start);
        assert!(!breaker.allow(start + Duration::from_secs(30)));

        breaker.record_failure(start + Duration::from_secs(30));
        // Cooldown restarted at t=30, so t=61 is still inside.
        assert!(!breaker.allow(start + Duration::from_secs(61)));
        assert!(breaker.allow(start + Duration::from_secs(91)));
    }

    #[test]
    fn set_tracks_backends_independently() {
        let start = Instant::now();
        let mut set = BreakerSet::new(1, 60);
        set.record_failure("a", start);
        assert_eq!(set.state("a"), BreakerState::Open);
        assert_eq!(set.state("b"), BreakerState::Closed);
        assert!(set.allow("b", start));
        set.record_success("a");
        assert_eq!(set.state("a"), BreakerState::Closed);
    }
}
