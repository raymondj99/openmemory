use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync + 'static {
    fn now_secs(&self) -> i64;

    fn now_millis(&self) -> i64 {
        self.now_secs() * 1000
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl SystemClock {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn shared() -> Arc<dyn Clock> {
        Arc::new(Self)
    }
}

impl Clock for SystemClock {
    fn now_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn now_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
}

#[derive(Debug, Clone)]
pub struct FixedClock {
    inner: Arc<std::sync::atomic::AtomicI64>,
}

impl FixedClock {
    #[must_use]
    pub fn new(seconds: i64) -> Self {
        Self {
            inner: Arc::new(std::sync::atomic::AtomicI64::new(seconds)),
        }
    }

    pub fn advance(&self, delta: i64) {
        self.inner
            .fetch_add(delta, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set(&self, seconds: i64) {
        self.inner
            .store(seconds, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now_secs(&self) -> i64 {
        self.inner.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_positive_seconds() {
        let clock = SystemClock::new();
        assert!(clock.now_secs() > 1_700_000_000);
    }

    #[test]
    fn system_clock_millis_consistent_with_secs() {
        let clock = SystemClock::new();
        let secs = clock.now_secs();
        let millis = clock.now_millis();
        assert!((millis - secs * 1000).abs() < 2_000);
    }

    #[test]
    fn fixed_clock_returns_pinned_value() {
        let clock = FixedClock::new(42);
        assert_eq!(clock.now_secs(), 42);
        assert_eq!(clock.now_millis(), 42_000);
    }

    #[test]
    fn fixed_clock_advance_moves_forward() {
        let clock = FixedClock::new(100);
        clock.advance(50);
        assert_eq!(clock.now_secs(), 150);
    }

    #[test]
    fn fixed_clock_set_replaces_value() {
        let clock = FixedClock::new(0);
        clock.set(2_000_000_000);
        assert_eq!(clock.now_secs(), 2_000_000_000);
    }

    #[test]
    fn fixed_clock_as_dyn_trait() {
        let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(5));
        assert_eq!(clock.now_secs(), 5);
    }
}
