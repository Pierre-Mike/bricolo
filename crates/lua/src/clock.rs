//! The CPU budget of one Lua state. A count hook checks it while Lua code
//! runs; every poll of a script coroutine starts the budget again, so time
//! spent waiting on a provider does not count.
//!
//! Lua code can catch any error with `pcall`, so an overrun has to end the
//! call some other way. The first check past the deadline raises an error.
//! If the code catches it and keeps running, later checks alternate between
//! yielding, which passes through `pcall` back to [`budgeted`], and raising
//! again for code that cannot yield. Either way [`budgeted`] then fails the
//! call and drops its coroutine.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Poll;
use std::time::Instant;

use mlua::VmState;

pub struct Clock {
    base: Instant,
    /// Milliseconds after `base` when the running code must stop.
    deadline: AtomicU64,
    /// The budget armed last, for the error message.
    budget: AtomicU64,
    /// Checks past the deadline since the budget was last armed.
    strikes: AtomicU64,
}

impl Clock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            base: Instant::now(),
            deadline: AtomicU64::new(u64::MAX),
            budget: AtomicU64::new(0),
            strikes: AtomicU64::new(0),
        })
    }

    fn now(&self) -> u64 {
        self.base.elapsed().as_millis() as u64
    }

    pub fn arm(&self, budget_ms: u64) {
        self.budget.store(budget_ms, Ordering::Relaxed);
        self.strikes.store(0, Ordering::Relaxed);
        self.deadline
            .store(self.now().saturating_add(budget_ms), Ordering::Relaxed);
    }

    fn error(&self) -> mlua::Error {
        let ms = self.budget.load(Ordering::Relaxed);
        mlua::Error::runtime(format!("ran past its {ms} ms budget"))
    }

    /// What the count hook does: nothing while within budget, then an
    /// error, then a yield, alternating.
    pub fn check(&self) -> mlua::Result<VmState> {
        if self.now() <= self.deadline.load(Ordering::Relaxed) {
            return Ok(VmState::Continue);
        }
        if self
            .strikes
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(2)
        {
            Err(self.error())
        } else {
            Ok(VmState::Yield)
        }
    }

    fn overrun(&self) -> bool {
        self.strikes.load(Ordering::Relaxed) > 0
    }
}

/// Runs `fut`, arming `budget_ms` before each poll. A poll that ran past
/// the budget fails the call, even if the Lua code caught the error.
pub async fn budgeted<T, F>(clock: Arc<Clock>, budget_ms: u64, fut: F) -> mlua::Result<T>
where
    F: Future<Output = mlua::Result<T>>,
{
    let mut fut = pin!(fut);
    std::future::poll_fn(|cx| {
        clock.arm(budget_ms);
        let poll = fut.as_mut().poll(cx);
        if clock.overrun() {
            return Poll::Ready(Err(clock.error()));
        }
        poll
    })
    .await
}
