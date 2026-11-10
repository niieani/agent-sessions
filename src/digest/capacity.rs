//! Server-wide model capacity. A 507 means the server cannot load the model at
//! all, so every worker pauses: one leader retries the same request once per
//! interval while the others wait, and the pass ends when the wait runs out.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use anyhow::Result;

use super::client::CapacityUnavailable;
use super::{ModelClient, ModelRequest};

/// How the capacity wait pauses; injectable so tests need not sleep.
pub trait CapacitySleep: Sync {
    fn sleep(&self, duration: Duration);
}

pub struct ThreadSleep;

impl CapacitySleep for ThreadSleep {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The capacity wait ran out; the Digest pass must end without failing items.
#[derive(Debug)]
pub(super) struct CapacityExhausted;

impl std::fmt::Display for CapacityExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Digest model capacity wait exhausted")
    }
}
impl std::error::Error for CapacityExhausted {}

pub(super) fn is_exhausted(error: &anyhow::Error) -> bool {
    error.downcast_ref::<CapacityExhausted>().is_some()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Available,
    Waiting,
    Exhausted,
}

/// A client wrapper that turns 507s into one shared, bounded wait.
pub(super) struct GatedClient<'a> {
    inner: &'a dyn ModelClient,
    sleep: &'a dyn CapacitySleep,
    interval: Duration,
    max_wait: Duration,
    state: Mutex<State>,
    changed: Condvar,
    /// Requests sent during waits, beyond the one each caller counts.
    extra_requests: AtomicUsize,
}

impl<'a> GatedClient<'a> {
    pub fn new(
        inner: &'a dyn ModelClient,
        sleep: &'a dyn CapacitySleep,
        interval: Duration,
        max_wait: Duration,
    ) -> Self {
        Self {
            inner,
            sleep,
            interval,
            max_wait,
            state: Mutex::new(State::Available),
            changed: Condvar::new(),
            extra_requests: AtomicUsize::new(0),
        }
    }

    pub fn exhausted(&self) -> bool {
        *self.state.lock().expect("capacity state poisoned") == State::Exhausted
    }

    pub fn extra_requests(&self) -> usize {
        self.extra_requests.load(Ordering::Relaxed)
    }

    pub fn max_wait(&self) -> Duration {
        self.max_wait
    }

    fn set(&self, state: State) {
        *self.state.lock().expect("capacity state poisoned") = state;
        self.changed.notify_all();
    }

    /// Block while another worker waits; fail once the wait ran out.
    fn admit(&self) -> Result<()> {
        let mut state = self.state.lock().expect("capacity state poisoned");
        while *state == State::Waiting {
            state = self.changed.wait(state).expect("capacity state poisoned");
        }
        anyhow::ensure!(*state != State::Exhausted, CapacityExhausted);
        Ok(())
    }

    /// Whether this caller leads the wait; others re-enter `admit`.
    fn begin_wait(&self) -> bool {
        let mut state = self.state.lock().expect("capacity state poisoned");
        let lead = *state == State::Available;
        if lead {
            *state = State::Waiting;
        }
        lead
    }

    fn lead_wait(&self, request: &ModelRequest, first: &CapacityUnavailable) -> Result<String> {
        tracing::warn!(
            detail = first.detail,
            retry_interval = ?self.interval,
            max_wait = ?self.max_wait,
            "Digest model capacity unavailable (HTTP 507); pausing Digest requests"
        );
        let mut waited = Duration::ZERO;
        while waited < self.max_wait {
            let step = self.interval.min(self.max_wait - waited);
            self.sleep.sleep(step);
            waited += step;
            self.extra_requests.fetch_add(1, Ordering::Relaxed);
            match self.inner.complete(request) {
                Err(error) if error.is::<CapacityUnavailable>() => {
                    tracing::info!(waited = ?waited, "Digest model capacity still unavailable");
                }
                result => {
                    tracing::info!(waited = ?waited, "Digest model capacity returned; resuming");
                    self.set(State::Available);
                    return result;
                }
            }
        }
        self.set(State::Exhausted);
        Err(CapacityExhausted.into())
    }
}

impl ModelClient for GatedClient<'_> {
    fn complete(&self, request: &ModelRequest) -> Result<String> {
        let mut resent = false;
        loop {
            self.admit()?;
            if resent {
                self.extra_requests.fetch_add(1, Ordering::Relaxed);
            }
            resent = true;
            match self.inner.complete(request) {
                Err(error) => match error.downcast_ref::<CapacityUnavailable>() {
                    Some(capacity) if self.begin_wait() => {
                        return self.lead_wait(request, capacity);
                    }
                    Some(_) => continue,
                    None => return Err(error),
                },
                ok => return ok,
            }
        }
    }
}
