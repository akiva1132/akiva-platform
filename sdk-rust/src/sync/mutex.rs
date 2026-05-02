//! Async mutex (Stone 11.6.8 #3).
//!
//! A futurized exclusive lock. `lock().await` parks until the held
//! [`MutexGuard`] is dropped; the guard derefs to the protected value
//! and releases on drop. Useful even in single-threaded wasm because
//! `TaskScope::spawn`-launched tasks share the event loop — two
//! tasks running interleaved can race on a `RefCell` if one holds
//! `borrow_mut` across an `.await`. A futurized mutex enforces the
//! exclusion across await points cleanly.
//!
//! ```ignore
//! use akiva_sdk::sync::Mutex;
//! use std::rc::Rc;
//!
//! let counter = Rc::new(Mutex::new(0i32));
//! let c1 = Rc::clone(&counter);
//! ctx.activity_scope().spawn(async move {
//!     let mut g = c1.lock().await;
//!     *g += 1;
//! });
//! let c2 = Rc::clone(&counter);
//! ctx.activity_scope().spawn(async move {
//!     let mut g = c2.lock().await;
//!     *g += 1;
//! });
//! ```

use std::cell::{Cell, RefCell, UnsafeCell};
use std::future::poll_fn;
use std::ops::{Deref, DerefMut};
use std::task::{Poll, Waker};

/// Async exclusive lock. Wrap shared state in `Rc<Mutex<T>>` to give
/// every cooperating task a clone of the lock handle.
pub struct Mutex<T> {
    locked: Cell<bool>,
    value: UnsafeCell<T>,
    waiters: RefCell<Vec<Waker>>,
}

/// RAII guard returned by [`Mutex::lock`] and [`Mutex::try_lock`].
/// Exclusive access ends when the guard is dropped — at which point
/// every parked `.lock()` future is woken so they can race for the
/// freshly-released lock.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    /// Construct a new mutex around `value`. The lock starts in the
    /// unlocked state. Wrap in `Rc<Mutex<T>>` to share between
    /// cooperating tasks.
    pub fn new(value: T) -> Self {
        Self {
            locked: Cell::new(false),
            value: UnsafeCell::new(value),
            waiters: RefCell::new(Vec::new()),
        }
    }

    /// Synchronous lock attempt. Returns `Some(guard)` if the lock
    /// was free, `None` if it's already held.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self.locked.get() {
            return None;
        }
        self.locked.set(true);
        Some(MutexGuard { mutex: self })
    }

    /// Async lock acquisition. Parks until the lock is released.
    pub fn lock(&self) -> impl std::future::Future<Output = MutexGuard<'_, T>> + '_ {
        poll_fn(move |cx| {
            if !self.locked.get() {
                self.locked.set(true);
                return Poll::Ready(MutexGuard { mutex: self });
            }
            self.waiters.borrow_mut().push(cx.waker().clone());
            Poll::Pending
        })
    }

    /// Whether the lock is currently held. Race-prone in concurrent
    /// settings but useful for assertions in single-threaded tests.
    pub fn is_locked(&self) -> bool {
        self.locked.get()
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: `locked == true` (set by acquire) ensures we hold
        // exclusive access; wasm is single-threaded so no other CPU
        // can race. The `&self` shared reference is the only path
        // outside the guard's lifetime.
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: same as `deref` — and we have `&mut self` so no
        // shared borrow can coexist within this guard's frame.
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.set(false);
        // Wake every parked waiter — see channel.rs for the rationale
        // (dropped futures may have stale wakers that never make
        // progress; waking everyone lets a live waiter through).
        for w in std::mem::take(&mut *self.mutex.waiters.borrow_mut()) {
            w.wake();
        }
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
    fn try_lock_releases_on_guard_drop() {
        let m = Mutex::new(5);
        {
            let mut g = m.try_lock().unwrap();
            *g += 1;
            assert_eq!(*g, 6);
            assert!(m.is_locked());
            assert!(m.try_lock().is_none(), "second try_lock must fail while held");
        }
        assert!(!m.is_locked());
        assert_eq!(*m.try_lock().unwrap(), 6);
    }

    #[test]
    fn contention_two_tasks_increment_counter() {
        let counter: Rc<Mutex<i32>> = Rc::new(Mutex::new(0));

        let m1 = Rc::clone(&counter);
        let mut t1: BoxFut<()> = Box::pin(async move {
            let mut g = m1.lock().await;
            *g += 1;
        });
        let m2 = Rc::clone(&counter);
        let mut t2: BoxFut<()> = Box::pin(async move {
            let mut g = m2.lock().await;
            *g += 1;
        });

        // Drive both alternately until both finish. Mutex ensures only
        // one holds the lock at a time; final counter == 2 proves no
        // double-mutation, no lost increment.
        let mut t1_done = false;
        let mut t2_done = false;
        for _ in 0..256 {
            if !t1_done {
                if let Poll::Ready(()) = poll_once(&mut t1) { t1_done = true; }
            }
            if !t2_done {
                if let Poll::Ready(()) = poll_once(&mut t2) { t2_done = true; }
            }
            if t1_done && t2_done { break; }
            let _ = take_wake_flag();
        }
        assert!(t1_done && t2_done, "both tasks should complete");
        assert_eq!(*counter.try_lock().unwrap(), 2);
    }

    #[test]
    fn lock_parks_when_held_and_wakes_on_release() {
        let m: Rc<Mutex<i32>> = Rc::new(Mutex::new(0));

        // First task holds the guard for the duration of the test.
        let g = m.try_lock().unwrap();
        assert!(m.is_locked());

        let m2 = Rc::clone(&m);
        let mut waiter: BoxFut<()> = Box::pin(async move {
            let mut g = m2.lock().await;
            *g = 99;
        });
        assert!(matches!(poll_once(&mut waiter), Poll::Pending));
        let _ = take_wake_flag();

        // Drop the held guard — should wake the waiter.
        drop(g);
        assert!(take_wake_flag(), "guard drop must set wake flag");
        assert!(!m.is_locked());

        assert!(matches!(poll_once(&mut waiter), Poll::Ready(())));
        assert_eq!(*m.try_lock().unwrap(), 99);
    }

    #[test]
    fn deref_and_deref_mut_thread_through_guard() {
        // Sanity check that the unsafe deref impls behave as
        // expected for non-Copy types.
        let m = Mutex::new(String::from("hello"));
        {
            let g = m.try_lock().unwrap();
            assert_eq!(g.len(), 5);
        }
        {
            let mut g = m.try_lock().unwrap();
            g.push_str(" world");
            assert_eq!(g.as_str(), "hello world");
        }
    }

}
