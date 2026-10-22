//! Spreads requests over tokens, with a minimum interval between request starts per token and
//! a cap on concurrent requests per token. Each request goes to the token that can start it
//! soonest, so a slow or rate-limited token does not hold up the others.
//!
//! A shared limit on requests at once backs off when Wit or the network is overloaded: it
//! halves the requests being sent when one times out or meets a server error, then grows by
//! about one for every `limit` successes (additive increase, multiplicative decrease, as in
//! TCP).

use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::Instant;

pub(crate) struct Scheduler {
    /// One permit per request booked or being sent, up to the limit.
    permits: Semaphore,
    state: Mutex<State>,
    per_token: usize,
    interval: Duration,
}

struct State {
    tokens: Vec<TokenState>,
    limit: Limit,
    /// Permits that remain once the owed ones are gone; follows the limit.
    permits: usize,
    /// Permits to drop when released, because the limit was cut below the permits in use.
    owed: usize,
    /// Requests being sent.
    sending: usize,
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

/// Requests allowed at once.
struct Limit {
    /// Fractional, so that each success can raise it by `1 / value`.
    value: f64,
    max: usize,
    /// When the limit was last cut. Failures of requests that started by then belong to the
    /// same overload, so they do not cut it again.
    last_cut: Option<Instant>,
}

impl Scheduler {
    pub fn new(tokens: usize, per_token: usize, interval: Duration) -> Self {
        let now = Instant::now();
        let max = tokens * per_token;
        Self {
            permits: Semaphore::new(max),
            state: Mutex::new(State {
                tokens: (0..tokens)
                    .map(|_| TokenState {
                        next_start: now,
                        after_started: now,
                        held_until: now,
                        holds: 0,
                        in_flight: 0,
                    })
                    .collect(),
                limit: Limit {
                    value: max as f64,
                    max,
                    last_cut: None,
                },
                permits: max,
                owed: 0,
                sending: 0,
            }),
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
        let permit = Permit {
            scheduler: self,
            permit: Some(permit),
        };
        loop {
            let (booking, start) = self.book();
            tokio::time::sleep_until(start).await;
            if self.start(&booking, start) {
                return Slot {
                    booking,
                    start,
                    _permit: permit,
                };
            }
            // The token was held while this request waited. Dropping the booking frees its
            // capacity; book again.
        }
    }

    /// Books the soonest start on a token with capacity.
    fn book(&self) -> (Booking<'_>, Instant) {
        let mut state = self.lock();
        let now = Instant::now();
        let (index, token) = state
            .tokens
            .iter_mut()
            .enumerate()
            .filter(|(_, token)| token.in_flight < self.per_token)
            .min_by_key(|(_, token)| token.next_start)
            .expect("a permit implies a token with capacity");
        let start = token.next_start.max(now);
        token.next_start = start + self.interval;
        token.in_flight += 1;
        let booking = Booking {
            scheduler: self,
            token: index,
            holds: token.holds,
        };
        (booking, start)
    }

    /// Records that `booking` starts at `start`, unless its token was held after it was booked.
    fn start(&self, booking: &Booking<'_>, start: Instant) -> bool {
        let mut state = self.lock();
        let token = &mut state.tokens[booking.token];
        if token.holds != booking.holds {
            return false;
        }
        token.after_started = token.after_started.max(start + self.interval);
        state.sending += 1;
        true
    }

    /// Delays the next request on `token`, e.g. after a `Retry-After` response. Requests
    /// already waiting for this token book again after the hold.
    pub fn hold(&self, token: usize, delay: Duration) {
        let until = Instant::now() + delay;
        self.lock().tokens[token].hold(until);
    }

    /// Delays the next request on every token, e.g. when an account-wide limit is reached.
    pub fn hold_all(&self, delay: Duration) {
        let until = Instant::now() + delay;
        for token in &mut self.lock().tokens {
            token.hold(until);
        }
    }

    /// Adds or removes permits to match the limit. Permits in use when the limit is cut are
    /// owed, and dropped as they are released.
    fn resize(&self, state: &mut State) {
        let allowed = state.limit.allowed();
        if allowed > state.permits {
            let added = allowed - state.permits;
            let repaid = added.min(state.owed);
            state.owed -= repaid;
            self.permits.add_permits(added - repaid);
        } else {
            let removed = state.permits - allowed;
            state.owed += removed - self.permits.forget_permits(removed);
        }
        state.permits = allowed;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
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

impl Limit {
    fn allowed(&self) -> usize {
        (self.value as usize).clamp(1, self.max)
    }

    fn succeeded(&mut self) {
        self.value = (self.value + 1.0 / self.value).min(self.max as f64);
    }

    /// Cuts the limit to half the requests being sent, unless the failed request started by
    /// the last cut. Returns the new limit when it was cut.
    fn overloaded(&mut self, started: Instant, sending: usize) -> Option<usize> {
        if self.last_cut.is_some_and(|cut| started <= cut) {
            return None;
        }
        self.value = self.value.min((sending as f64 / 2.0).max(1.0));
        self.last_cut = Some(Instant::now());
        Some(self.allowed())
    }
}

/// A semaphore permit that is dropped instead of released while the limit is owed permits.
struct Permit<'a> {
    scheduler: &'a Scheduler,
    permit: Option<SemaphorePermit<'a>>,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut state = self.scheduler.lock();
        if state.owed > 0 {
            state.owed -= 1;
            if let Some(permit) = self.permit.take() {
                permit.forget();
            }
        }
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
        self.scheduler.lock().tokens[self.token].in_flight -= 1;
    }
}

/// A request slot that may start now; releases its token capacity when dropped.
pub(crate) struct Slot<'a> {
    booking: Booking<'a>,
    start: Instant,
    _permit: Permit<'a>,
}

impl Slot<'_> {
    /// Index of the token this slot belongs to.
    pub fn token(&self) -> usize {
        self.booking.token
    }

    /// Reports that the request succeeded, so more may run at once.
    pub fn succeeded(&self) {
        let scheduler = self.booking.scheduler;
        let mut state = scheduler.lock();
        state.limit.succeeded();
        scheduler.resize(&mut state);
    }

    /// Reports that the request failed in a way that suggests Wit or the network is
    /// overloaded, so fewer may run at once. Returns the new limit when it was cut.
    pub fn overloaded(&self) -> Option<usize> {
        let scheduler = self.booking.scheduler;
        let mut state = scheduler.lock();
        let sending = state.sending;
        let cut = state.limit.overloaded(self.start, sending);
        scheduler.resize(&mut state);
        cut
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.booking.scheduler.lock().sending -= 1;
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

    /// Whether another request could start within a second.
    async fn has_room(scheduler: &Scheduler) -> bool {
        tokio::time::timeout(Duration::from_secs(1), scheduler.acquire())
            .await
            .is_ok()
    }

    #[tokio::test(start_paused = true)]
    async fn overloads_halve_the_requests_being_sent() {
        let scheduler = Scheduler::new(1, 8, Duration::ZERO);
        let mut slots = Vec::new();
        for _ in 0..8 {
            slots.push(scheduler.acquire().await);
        }
        assert_eq!(slots[0].overloaded(), Some(4));
        // Started before the cut: the same overload.
        assert_eq!(slots[1].overloaded(), None);
        slots.clear();

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..4 {
            slots.push(scheduler.acquire().await);
        }
        assert!(!has_room(&scheduler).await);
        // Started after the cut, so it cuts again.
        assert_eq!(slots[0].overloaded(), Some(2));
        slots.truncate(1);
        assert_eq!(slots[0].overloaded(), None);
        slots.clear();
        let a = scheduler.acquire().await;
        let b = scheduler.acquire().await;
        assert!(!has_room(&scheduler).await);
        // Never below one request at once.
        drop(b);
        tokio::time::advance(Duration::from_millis(1)).await;
        let c = scheduler.acquire().await;
        drop(a);
        assert_eq!(c.overloaded(), Some(1));
        assert!(!has_room(&scheduler).await);
    }

    #[tokio::test(start_paused = true)]
    async fn successes_raise_the_limit_again() {
        let scheduler = Scheduler::new(1, 8, Duration::ZERO);
        let mut slots = Vec::new();
        for _ in 0..8 {
            slots.push(scheduler.acquire().await);
        }
        slots[0].overloaded();
        slots.truncate(4);
        // About one more request at once for every `limit` successes: 4.25, 4.49, 4.71, 4.92.
        for _ in 0..4 {
            slots[0].succeeded();
        }
        assert!(!has_room(&scheduler).await);
        slots[0].succeeded();
        let _fifth = scheduler.acquire().await;
        assert!(!has_room(&scheduler).await);
        for _ in 0..100 {
            slots[0].succeeded();
        }
        let mut more = Vec::new();
        for _ in 0..3 {
            more.push(scheduler.acquire().await);
        }
        // Back to the configured cap.
        assert!(!has_room(&scheduler).await);
    }
}
