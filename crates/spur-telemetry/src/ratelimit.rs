use std::time::Instant;

pub(crate) const EVENTS_PER_MINUTE_CAPACITY: u32 = 500;
pub(crate) const EVENTS_PER_SECOND_REFILL: f64 = 500.0 / 60.0;

pub(crate) struct TokenBucket<C = fn() -> Instant>
where
    C: Fn() -> Instant,
{
    capacity: u32,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
    clock: C,
}

impl TokenBucket<fn() -> Instant> {
    pub(crate) fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self::new_with_clock(capacity, refill_per_sec, Instant::now)
    }
}

impl<C> TokenBucket<C>
where
    C: Fn() -> Instant,
{
    pub(crate) fn new_with_clock(capacity: u32, refill_per_sec: f64, clock: C) -> Self {
        let now = clock();
        Self {
            capacity,
            refill_per_sec,
            tokens: f64::from(capacity),
            last_refill: now,
            clock,
        }
    }

    pub(crate) fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = (self.clock)();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }

        let refilled = self.tokens + elapsed * self.refill_per_sec;
        self.tokens = refilled.min(f64::from(self.capacity));
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn starts_full() {
        let mut bucket = TokenBucket::new(EVENTS_PER_MINUTE_CAPACITY, EVENTS_PER_SECOND_REFILL);

        for _ in 0..EVENTS_PER_MINUTE_CAPACITY {
            assert!(bucket.try_acquire());
        }
        assert!(!bucket.try_acquire());
    }

    #[test]
    fn refills_after_elapsed_time() {
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let clock = {
            let now = Rc::clone(&now);
            move || now.get()
        };

        let mut bucket = TokenBucket::new_with_clock(2, 4.0, clock);
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(!bucket.try_acquire());

        now.set(start + Duration::from_millis(250));
        assert!(bucket.try_acquire());
        assert!(!bucket.try_acquire());
    }

    #[test]
    fn refill_never_exceeds_capacity() {
        let start = Instant::now();
        let now = Rc::new(Cell::new(start));
        let clock = {
            let now = Rc::clone(&now);
            move || now.get()
        };

        let mut bucket = TokenBucket::new_with_clock(3, 10.0, clock);
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(!bucket.try_acquire());

        now.set(start + Duration::from_secs(10));
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(!bucket.try_acquire());
    }
}
