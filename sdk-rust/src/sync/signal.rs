//! Single-slot, multi-waiter signal (Stone 11.6.8 #3).
//!
//! `Signal<T: Clone>` is the async analogue of Compose's
//! `mutableStateOf` or SwiftUI's `@Published`: every `wait().await`
//! parks until the next [`Signal::set`] call wakes it, at which point
//! it returns the freshly-set value. Multiple waiters all wake on a
//! single set.
//!
//! Difference from a channel: a signal has no buffer and no
//! per-receiver value retention. Late waiters miss earlier `set`s
//! unless they call [`Signal::current`] first. This is exactly what
//! you want for "observable state" — the latest value matters, not
//! the history.
//!
//! ```ignore
//! use akiva_sdk::sync::Signal;
//! use std::rc::Rc;
//!
//! let theme: Rc<Signal<&'static str>> = Rc::new(Signal::new());
//!
//! let observer = Rc::clone(&theme);
//! ctx.activity_scope().spawn(async move {
//!     loop {
//!         let next = observer.wait().await;
//!         akiva_sdk::log::info!("theme is now {next}");
//!     }
//! });
//!
//! // Elsewhere, in response to a settings change:
//! theme.set("dark");
//! ```

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::task::{Poll, Waker};

/// Multi-waiter notification primitive. `T: Clone` because every
/// waiter receives its own copy of the value when [`Signal::set`]
/// fires.
pub struct Signal<T: Clone> {
    value: RefCell<Option<T>>,
    /// Monotonic counter incremented on every `set`. A waiter
    /// registered at version `V` wakes iff the current version
    /// exceeds `V`.
    version: Cell<u64>,
    waiters: RefCell<Vec<Waker>>,
}

impl<T: Clone> Signal<T> {
    pub fn new() -> Self {
        Self {
            value: RefCell::new(None),
            version: Cell::new(0),
            waiters: RefCell::new(Vec::new()),
        }
    }

    /// Construct with an initial value. Equivalent to
    /// `let s = Signal::new(); s.set(v); s` but bumps the version
    /// once instead of starting at zero with a seeded value.
    pub fn with(value: T) -> Self {
        Self {
            value: RefCell::new(Some(value)),
            version: Cell::new(1),
            waiters: RefCell::new(Vec::new()),
        }
    }

    /// Synchronous read of the latest value. Returns `None` if `set`
    /// has never been called.
    pub fn current(&self) -> Option<T> {
        self.value.borrow().clone()
    }

    /// Publish a new value. Wakes every parked [`Signal::wait`]
    /// future; their next poll observes the new version and resolves
    /// to a clone of the value.
    pub fn set(&self, value: T) {
        *self.value.borrow_mut() = Some(value);
        self.version.set(self.version.get() + 1);
        for w in std::mem::take(&mut *self.waiters.borrow_mut()) {
            w.wake();
        }
    }

    /// Async wait for the NEXT change. The returned future resolves
    /// only after a `set` strictly subsequent to the call to `wait`
    /// — if you want "the current value or wait if absent," call
    /// [`Signal::current`] first and fall back to `wait` on `None`.
    pub fn wait(&self) -> impl std::future::Future<Output = T> + '_ {
        let start_version = self.version.get();
        poll_fn(move |cx| {
            if self.version.get() > start_version {
                if let Some(v) = self.value.borrow().as_ref() {
                    return Poll::Ready(v.clone());
                }
            }
            self.waiters.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
    }
}

impl<T: Clone> Default for Signal<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{poll_once, take_wake_flag};
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;

    type BoxFut<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

    #[test]
    fn current_returns_none_before_first_set() {
        let s: Signal<i32> = Signal::new();
        assert_eq!(s.current(), None);
        s.set(42);
        assert_eq!(s.current(), Some(42));
    }

    #[test]
    fn with_starts_at_seeded_value() {
        let s: Signal<&'static str> = Signal::with("dark");
        assert_eq!(s.current(), Some("dark"));
    }

    #[test]
    fn three_waiters_all_wake_on_single_set() {
        let s: Rc<Signal<i32>> = Rc::new(Signal::new());

        let make_waiter = || -> BoxFut<i32> {
            let s = Rc::clone(&s);
            Box::pin(async move { s.wait().await })
        };
        let mut a = make_waiter();
        let mut b = make_waiter();
        let mut c = make_waiter();

        // First poll on each parks them.
        assert!(matches!(poll_once(&mut a), Poll::Pending));
        assert!(matches!(poll_once(&mut b), Poll::Pending));
        assert!(matches!(poll_once(&mut c), Poll::Pending));
        let _ = take_wake_flag();

        s.set(7);
        assert!(take_wake_flag(), "set must set wake flag");

        // All three resolve to the same value.
        match poll_once(&mut a) { Poll::Ready(v) => assert_eq!(v, 7), Poll::Pending => panic!("a") }
        match poll_once(&mut b) { Poll::Ready(v) => assert_eq!(v, 7), Poll::Pending => panic!("b") }
        match poll_once(&mut c) { Poll::Ready(v) => assert_eq!(v, 7), Poll::Pending => panic!("c") }
    }

    #[test]
    fn wait_returns_only_for_next_change_not_current_value() {
        let s: Rc<Signal<i32>> = Rc::new(Signal::with(0));

        let s2 = Rc::clone(&s);
        let mut w: BoxFut<i32> = Box::pin(async move { s2.wait().await });
        // Even though current is Some(0), wait() blocks for the next set.
        assert!(matches!(poll_once(&mut w), Poll::Pending));
        let _ = take_wake_flag();

        s.set(99);
        match poll_once(&mut w) {
            Poll::Ready(v) => assert_eq!(v, 99),
            Poll::Pending => panic!("expected Ready after set"),
        }
    }

    #[test]
    fn version_bumps_advance_independently_for_late_waiters() {
        // A waiter registered AFTER one set should still block until
        // the NEXT set, not return immediately on the existing value.
        let s: Rc<Signal<i32>> = Rc::new(Signal::new());
        s.set(1);
        // Late-arriving waiter:
        let s2 = Rc::clone(&s);
        let mut late: BoxFut<i32> = Box::pin(async move { s2.wait().await });
        assert!(matches!(poll_once(&mut late), Poll::Pending));
        let _ = take_wake_flag();

        s.set(2);
        match poll_once(&mut late) {
            Poll::Ready(v) => assert_eq!(v, 2),
            Poll::Pending => panic!("expected Ready after second set"),
        }
    }
}
