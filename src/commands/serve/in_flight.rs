//! The requests being answered, so a re-exec for an upgrade waits for them:
//! a merge cut off midway leaves its feature half removed. Event streams
//! are not counted; they never end on their own, and the app reconnects.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct InFlight {
    count: Mutex<usize>,
    idle: Condvar,
}

/// One request being answered, counted until dropped.
pub(super) struct Answering(Arc<InFlight>);

impl Drop for Answering {
    fn drop(&mut self) {
        let mut count = self.0.lock();
        *count -= 1;
        if *count == 0 {
            self.0.idle.notify_all();
        }
    }
}

impl InFlight {
    fn lock(&self) -> MutexGuard<'_, usize> {
        self.count.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn begin(self: &Arc<Self>) -> Answering {
        *self.lock() += 1;
        Answering(Arc::clone(self))
    }

    /// Wait until no request is being answered, or `limit` has passed;
    /// returns whether none is. While the guard is held, none can begin.
    pub(super) fn wait_idle(&self, limit: Duration) -> (MutexGuard<'_, usize>, bool) {
        let until = Instant::now() + limit;
        let mut count = self.lock();
        while *count > 0 {
            let Some(left) = until.checked_duration_since(Instant::now()) else {
                return (count, false);
            };
            count = self
                .idle
                .wait_timeout(count, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        (count, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn waits_until_the_requests_being_answered_are() {
        let in_flight = Arc::new(InFlight::default());
        let (finish, finished) = mpsc::channel::<()>();
        let (begun, has_begun) = mpsc::channel();
        let answering = std::thread::spawn({
            let answering = in_flight.begin();
            move || {
                let _answering = answering;
                begun.send(()).unwrap();
                finished.recv().unwrap();
            }
        });
        has_begun.recv().unwrap();

        assert!(!in_flight.wait_idle(Duration::from_millis(50)).1);

        finish.send(()).unwrap();
        assert!(in_flight.wait_idle(Duration::from_secs(10)).1);
        answering.join().unwrap();
    }
}
