//! Spreads requests over tokens, with a minimum interval between request starts per token and
//! a cap on concurrent requests per token. Each request goes to the token that can start it
//! soonest, so a slow or rate-limited token does not hold up the others.
//!
//! A shared limit on requests at once backs off when Wit or the network is overloaded: it
//! halves the requests being sent when one times out or meets a server error, then grows by
//! about one for every `limit` successes (additive increase, multiplicative decrease, as in
//! TCP).

use std::fmt;
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
    /// Cuts so far. A failed request cuts the limit only if no cut happened since it started
    /// sending; failures of requests that were already being sent belong to the same overload.
    cuts: u64,
}

/// What a booking does once its start time comes.
enum Start {
    /// Send now; holds the limit's cut count.
    Now(u64),
    /// The token was held after the booking: book again.
    Held,
    /// The limit was cut after the booking, and as many requests as it allows are being sent:
    /// wait for a permit again.
    Full,
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
                    cuts: 0,
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
        loop {
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
                match self.start(&booking, start) {
                    Start::Now(cuts) => {
                        return Slot {
                            booking,
                            cuts,
                            _permit: permit,
                        };
                    }
                    // Dropping the booking frees its token capacity; book again.
                    Start::Held => {}
                    // The limit was cut while this request waited. Give back the booking and
                    // the permit, which the cut takes out of use, and queue for a permit again.
                    // Booking afresh then keeps the request interval on the token.
                    Start::Full => break,
                }
            }
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

    /// Records that `booking` starts at `start`, unless its token was held after it was booked
    /// or the limit is reached.
    fn start(&self, booking: &Booking<'_>, start: Instant) -> Start {
        let mut state = self.lock();
        if state.sending >= state.limit.allowed() {
            return Start::Full;
        }
        let token = &mut state.tokens[booking.token];
        if token.holds != booking.holds {
            return Start::Held;
        }
        token.after_started = token.after_started.max(start + self.interval);
        state.sending += 1;
        Start::Now(state.limit.cuts)
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

    /// Requests in the scheduler now, for logs.
    pub fn load(&self) -> Load {
        let state = self.lock();
        let booked: usize = state.tokens.iter().map(|token| token.in_flight).sum();
        Load {
            sending: state.sending,
            waiting: booked - state.sending,
            limit: state.limit.allowed(),
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

    /// Cuts the limit to half the requests being sent, unless it was cut since the failed
    /// request started, when `cuts` cuts had happened. Returns the new limit when it was cut.
    fn overloaded(&mut self, cuts: u64, sending: usize) -> Option<usize> {
        if cuts != self.cuts {
            return None;
        }
        self.value = self.value.min((sending as f64 / 2.0).max(1.0));
        self.cuts += 1;
        Some(self.allowed())
    }
}

/// Requests being sent and waiting for their start, and the limit on both together.
pub(crate) struct Load {
    sending: usize,
    waiting: usize,
    limit: usize,
}

impl fmt::Display for Load {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} sending, {} waiting, at most {} at once",
            self.sending, self.waiting, self.limit
        )
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
    /// The limit's cut count when the request started.
    cuts: u64,
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
        let cut = state.limit.overloaded(self.cuts, sending);
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

    #[tokio::test(start_paused = true)]
    async fn cuts_reach_requests_waiting_for_their_start() {
        let scheduler = Arc::new(Scheduler::new(1, 8, Duration::from_secs(1)));
        let first = scheduler.acquire().await;
        // Seven more requests book starts at 1s to 7s, then stay in flight for 30s each.
        let tasks: Vec<_> = (0..7)
            .map(|_| {
                let scheduler = scheduler.clone();
                tokio::spawn(async move {
                    let _slot = scheduler.acquire().await;
                    let sending = scheduler.lock().sending;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    sending
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(first.overloaded(), Some(1));
        drop(first);
        let mut most = 0;
        for task in tasks {
            most = most.max(task.await.unwrap());
        }
        assert_eq!(most, 1, "requests sent at once after the cut");
    }

    #[tokio::test(start_paused = true)]
    async fn requests_that_start_late_after_a_cut_can_cut_again() {
        let scheduler = Scheduler::new(1, 8, Duration::from_secs(1));
        let t0 = Instant::now();
        let mut slots = Vec::new();
        for _ in 0..4 {
            slots.push(scheduler.acquire().await);
        }
        // Booked for 4s, but not polled again until after the cut at 5s.
        let mut late = std::pin::pin!(scheduler.acquire());
        assert!(futures::poll!(&mut late).is_pending());
        tokio::time::sleep_until(t0 + Duration::from_secs(5)).await;
        assert_eq!(slots[0].overloaded(), Some(2));
        slots.clear();
        tokio::time::advance(Duration::from_millis(1)).await;
        let late = late.await;
        assert_eq!(late.overloaded(), Some(1));
    }
}
