//! Exponential reconnect backoff with jitter.

use std::time::Duration;

pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self { base, max, attempt: 0 }
    }

    pub fn next_delay(&mut self) -> Duration {
        let exp = self.base.saturating_mul(1u32 << self.attempt.min(16));
        self.attempt = self.attempt.saturating_add(1);
        // ±20 % so that many clients coming back from an outage don't retry in lockstep.
        exp.min(self.max).mul_f64(0.8 + fastrand::f64() * 0.4)
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_and_caps() {
        let mut b = Backoff::new(Duration::from_secs(2), Duration::from_secs(60));
        let first = b.next_delay();
        assert!(first >= Duration::from_millis(1600) && first <= Duration::from_millis(2400));
        for _ in 0..20 {
            assert!(b.next_delay() <= Duration::from_secs(72));
        }
        b.reset();
        assert!(b.next_delay() <= Duration::from_millis(2400));
    }
}
