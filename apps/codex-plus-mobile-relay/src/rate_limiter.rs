use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Token bucket rate limiter keyed by client IP.
///
/// Guards the WebSocket handshake path so a scanner cannot brute-force room
/// identifiers. Buckets are pruned lazily to keep memory bounded.
pub struct RateLimiter {
    buckets: HashMap<IpAddr, TokenBucket>,
    capacity: f64,
    refill_per_second: f64,
    last_cleanup: Instant,
}

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    /// `capacity` handshakes may burst, refilling at `capacity / window` per second.
    pub fn new(capacity: f64, window: Duration) -> Self {
        let window_secs = window.as_secs_f64().max(1.0);
        Self {
            buckets: HashMap::new(),
            capacity,
            refill_per_second: capacity / window_secs,
            last_cleanup: Instant::now(),
        }
    }

    pub fn check_and_consume(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let capacity = self.capacity;
        let refill = self.refill_per_second;
        let bucket = self.buckets.entry(ip).or_insert_with(|| TokenBucket {
            tokens: capacity,
            last_refill: now,
        });
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill).min(capacity);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn cleanup_stale(&mut self) {
        let now = Instant::now();
        if now.saturating_duration_since(self.last_cleanup) < Duration::from_secs(300) {
            return;
        }
        self.buckets.retain(|_, bucket| {
            now.saturating_duration_since(bucket.last_refill) < Duration::from_secs(600)
        });
        self.last_cleanup = now;
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(10.0, Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, last))
    }

    #[test]
    fn allows_up_to_capacity_then_blocks() {
        let mut limiter = RateLimiter::new(3.0, Duration::from_secs(60));
        assert!(limiter.check_and_consume(ip(1)));
        assert!(limiter.check_and_consume(ip(1)));
        assert!(limiter.check_and_consume(ip(1)));
        assert!(!limiter.check_and_consume(ip(1)));
    }

    #[test]
    fn tracks_each_ip_independently() {
        let mut limiter = RateLimiter::new(1.0, Duration::from_secs(60));
        assert!(limiter.check_and_consume(ip(1)));
        assert!(!limiter.check_and_consume(ip(1)));
        assert!(limiter.check_and_consume(ip(2)));
    }
}
