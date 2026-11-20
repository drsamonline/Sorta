//! A global move limiter.
//!
//! The spec puts a ceiling of 20 files per second on the whole process, so a bulk
//! scan cannot saturate a disk or a network share. This is a fixed-window counter,
//! not a token bucket: the point is "do not hammer the filesystem", and a window is
//! the simplest thing that does that without a background thread.
//!
//! `0` disables the limiter entirely, which is what the tests use so they do not
//! pay for a sleep they are not measuring.

use std::time::{Duration, Instant};

/// Default ceiling, from the spec.
pub const DEFAULT_PER_SECOND: u32 = 20;

/// Counts moves and sleeps when the current second is full.
#[derive(Debug)]
pub struct RateLimiter {
    per_second: u32,
    window_start: Instant,
    count: u32,
}

impl RateLimiter {
    pub fn new(per_second: u32) -> Self {
        Self {
            per_second,
            window_start: Instant::now(),
            count: 0,
        }
    }

    /// True when the limiter is active at all.
    pub fn is_enabled(&self) -> bool {
        self.per_second > 0
    }

    pub fn per_second(&self) -> u32 {
        self.per_second
    }

    /// Blocks until this move is allowed, then counts it.
    pub fn acquire(&mut self) {
        if !self.is_enabled() {
            return;
        }

        let now = Instant::now();
        let elapsed = now.duration_since(self.window_start);
        if elapsed >= Duration::from_secs(1) {
            self.window_start = now;
            self.count = 0;
        }

        if self.count >= self.per_second {
            let wait = Duration::from_secs(1).saturating_sub(elapsed);
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
            self.window_start = Instant::now();
            self.count = 0;
        }

        self.count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_disables_the_limiter() {
        let mut limiter = RateLimiter::new(0);
        assert!(!limiter.is_enabled());

        let start = Instant::now();
        for _ in 0..100 {
            limiter.acquire();
        }
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn the_ceiling_is_respected() {
        // One per second: the second call has to wait for the next window.
        let mut limiter = RateLimiter::new(1);
        let start = Instant::now();

        limiter.acquire();
        limiter.acquire();

        assert!(
            start.elapsed() >= Duration::from_millis(900),
            "second move should have waited, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn the_default_matches_the_spec() {
        assert_eq!(RateLimiter::new(DEFAULT_PER_SECOND).per_second(), 20);
    }
}
