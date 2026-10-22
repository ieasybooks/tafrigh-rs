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
    /// When the next booked request may start.
    next_start: Instant,
    /// When a request may start after the last one that did start.
    after_started: Instant,
    /// End of the latest hold.
    held_until: Instant,
    /// Holds so far; a booking made before a hold must book again.
    holds: u64,
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
                        after_started: now,
                        held_until: now,
                        holds: 0,
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
        loop {
            let (booking, start) = self.book();
            tokio::time::sleep_until(start).await;
            if self.start(&booking, start) {
                return Slot {
                    booking,
                    _permit: permit,
                };
            }
            // The token was held while this request waited. Dropping the booking frees its
            // capacity; book again.
        }
    }

    /// Books the soonest start on a token with capacity.
    fn book(&self) -> (Booking<'_>, Instant) {
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
        let booking = Booking {
            scheduler: self,
            token,
            holds: state.holds,
        };
        (booking, start)
    }

    /// Records that `booking` starts at `start`, unless its token was held after it was booked.
    fn start(&self, booking: &Booking<'_>, start: Instant) -> bool {
        let mut tokens = self.lock();
        let state = &mut tokens[booking.token];
        if state.holds != booking.holds {
            return false;
        }
        state.after_started = state.after_started.max(start + self.interval);
        true
    }

    /// Delays the next request on `token`, e.g. after a `Retry-After` response. Requests
    /// already waiting for this token book again after the hold.
    pub fn hold(&self, token: usize, delay: Duration) {
        let until = Instant::now() + delay;
        self.lock()[token].hold(until);
    }

    /// Delays the next request on every token, e.g. when an account-wide limit is reached.
    pub fn hold_all(&self, delay: Duration) {
        let until = Instant::now() + delay;
        for state in self.lock().iter_mut() {
            state.hold(until);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<TokenState>> {
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl TokenState {
    /// Every booking made so far books again, from the end of the hold but no sooner than
    /// the request interval after the last request that started.
    fn hold(&mut self, until: Instant) {
        self.held_until = self.held_until.max(until);
        self.next_start = self.held_until.max(self.after_started);
        self.holds += 1;
    }
}

/// A booked start on a token; releases the token's capacity when dropped.
struct Booking<'a> {
    scheduler: &'a Scheduler,
    token: usize,
    /// The token's hold count when booked.
    holds: u64,
}

impl Drop for Booking<'_> {
    fn drop(&mut self) {
        self.scheduler.lock()[self.token].in_flight -= 1;
    }
}

/// A request slot that may start now; releases its token capacity when dropped.
pub(crate) struct Slot<'a> {
    booking: Booking<'a>,
    _permit: SemaphorePermit<'a>,
}

impl Slot<'_> {
    /// Index of the token this slot belongs to.
    pub fn token(&self) -> usize {
        self.booking.token
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Books `count` requests that wait for their start; resolves to each one's token and start
    /// in milliseconds after `t0`, in start order.
    fn waiting(
        scheduler: &Arc<Scheduler>,
        count: usize,
        t0: Instant,
    ) -> impl Future<Output = Vec<(usize, u128)>> {
        let tasks: Vec<_> = (0..count)
            .map(|_| {
                let scheduler = scheduler.clone();
                tokio::spawn(async move {
                    let slot = scheduler.acquire().await;
                    (slot.token(), (Instant::now() - t0).as_millis())
                })
            })
            .collect();
        async move {
            let mut starts = Vec::new();
            for task in tasks {
                starts.push(task.await.unwrap());
            }
            starts.sort_by_key(|&(token, at)| (at, token));
            starts
        }
    }

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

    #[tokio::test(start_paused = true)]
    async fn holds_reach_requests_already_waiting() {
        let scheduler = Arc::new(Scheduler::new(1, 4, Duration::from_secs(1)));
        let t0 = Instant::now();
        let first = scheduler.acquire().await;
        // Booked for 1s, 2s and 3s.
        let starts = waiting(&scheduler, 3, t0);
        tokio::time::sleep(Duration::from_millis(10)).await;
        scheduler.hold(first.token(), Duration::from_secs(5));
        assert_eq!(starts.await, [(0, 5010), (0, 6010), (0, 7010)]);
    }

    #[tokio::test(start_paused = true)]
    async fn short_holds_delay_waiting_requests_only_as_needed() {
        let scheduler = Arc::new(Scheduler::new(1, 4, Duration::from_secs(1)));
        let t0 = Instant::now();
        let _first = scheduler.acquire().await;
        let starts = waiting(&scheduler, 3, t0);
        tokio::time::sleep(Duration::from_millis(10)).await;
        scheduler.hold(0, Duration::from_millis(1500));
        assert_eq!(starts.await, [(0, 1510), (0, 2510), (0, 3510)]);
    }

    #[tokio::test(start_paused = true)]
    async fn requests_waiting_on_a_held_token_move_to_another() {
        let scheduler = Arc::new(Scheduler::new(2, 4, Duration::from_secs(1)));
        let t0 = Instant::now();
        let _first = scheduler.acquire().await;
        let _second = scheduler.acquire().await;
        // Booked for 1s on each token.
        let starts = waiting(&scheduler, 2, t0);
        tokio::time::sleep(Duration::from_millis(10)).await;
        scheduler.hold(0, Duration::from_secs(5));
        assert_eq!(starts.await, [(1, 1000), (1, 2000)]);
    }

    #[tokio::test(start_paused = true)]
    async fn account_holds_reach_requests_waiting_on_every_token() {
        let scheduler = Arc::new(Scheduler::new(2, 4, Duration::from_secs(1)));
        let t0 = Instant::now();
        let _first = scheduler.acquire().await;
        let _second = scheduler.acquire().await;
        // Booked for 1s and 2s on each token.
        let starts = waiting(&scheduler, 4, t0);
        tokio::time::sleep(Duration::from_millis(10)).await;
        scheduler.hold_all(Duration::from_secs(3));
        assert_eq!(starts.await, [(0, 3010), (1, 3010), (0, 4010), (1, 4010)]);
    }
}
