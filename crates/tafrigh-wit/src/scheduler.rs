//! Spreads requests over tokens, with a minimum interval between request starts per token and
//! a cap on concurrent requests per token. Each request goes to the token that can start it
//! soonest, so a slow or rate-limited token does not hold up the others.

use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::Instant;

pub(crate) struct Scheduler {
    permits: Semaphore,
    tokens: Mutex<Vec<TokenState>>,
    per_token: usize,
    interval: Duration,
}

struct TokenState {
    next_start: Instant,
    in_flight: usize,
}

impl Scheduler {
    pub fn new(tokens: usize, per_token: usize, interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            permits: Semaphore::new(tokens * per_token),
            tokens: Mutex::new(
                (0..tokens)
                    .map(|_| TokenState {
                        next_start: now,
                        in_flight: 0,
                    })
                    .collect(),
            ),
            per_token,
            interval,
        }
    }

    /// Waits for a token that may start a request now.
    pub async fn acquire(&self) -> Slot<'_> {
        let permit = self
            .permits
            .acquire()
            .await
            .expect("semaphore is never closed");
        let (token, start) = {
            let mut tokens = self.lock();
            let now = Instant::now();
            let (token, state) = tokens
                .iter_mut()
                .enumerate()
                .filter(|(_, state)| state.in_flight < self.per_token)
                .min_by_key(|(_, state)| state.next_start)
                .expect("a permit implies a token with capacity");
            let start = state.next_start.max(now);
            state.next_start = start + self.interval;
            state.in_flight += 1;
            (token, start)
        };
        let slot = Slot {
            scheduler: self,
            token,
            _permit: permit,
        };
        tokio::time::sleep_until(start).await;
        slot
    }

    /// Delays the next request on `token`, e.g. after a `Retry-After` response.
    pub fn hold(&self, token: usize, delay: Duration) {
        let until = Instant::now() + delay;
        let mut tokens = self.lock();
        let state = &mut tokens[token];
        state.next_start = state.next_start.max(until);
    }

    /// Delays the next request on every token, e.g. when an account-wide limit is reached.
    pub fn hold_all(&self, delay: Duration) {
        let until = Instant::now() + delay;
        for state in self.lock().iter_mut() {
            state.next_start = state.next_start.max(until);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TokenState>> {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A reserved request slot; releases its token capacity when dropped.
pub(crate) struct Slot<'a> {
    scheduler: &'a Scheduler,
    token: usize,
    _permit: SemaphorePermit<'a>,
}

impl Slot<'_> {
    /// Index of the token this slot belongs to.
    pub fn token(&self) -> usize {
        self.token
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.scheduler.lock()[self.token].in_flight -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn spaces_starts_per_token_and_balances() {
        let scheduler = Scheduler::new(2, 4, Duration::from_secs(1));
        let t0 = Instant::now();
        let mut starts = Vec::new();
        for _ in 0..6 {
            let slot = scheduler.acquire().await;
            starts.push((slot.token(), (Instant::now() - t0).as_millis()));
        }
        // Two tokens alternate; each token starts at most once per second.
        assert_eq!(
            starts,
            vec![(0, 0), (1, 0), (0, 1000), (1, 1000), (0, 2000), (1, 2000)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn holds_one_token_or_all() {
        let scheduler = Scheduler::new(2, 4, Duration::ZERO);
        let t0 = Instant::now();
        scheduler.hold(0, Duration::from_secs(3));
        let slot = scheduler.acquire().await;
        assert_eq!((slot.token(), (Instant::now() - t0).as_millis()), (1, 0));

        scheduler.hold_all(Duration::from_secs(5));
        let slot = scheduler.acquire().await;
        assert_eq!(
            (Instant::now() - t0).as_millis(),
            5000,
            "token {}",
            slot.token()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn caps_in_flight_per_token() {
        let scheduler = Scheduler::new(1, 2, Duration::ZERO);
        let a = scheduler.acquire().await;
        let _b = scheduler.acquire().await;
        let third = tokio::time::timeout(Duration::from_secs(5), scheduler.acquire()).await;
        assert!(third.is_err(), "third request must wait for capacity");
        drop(a);
        let _c = scheduler.acquire().await;
    }
}
