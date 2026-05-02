//! Bounded async channel (Stone 11.6.8 #3).
//!
//! `Channel<T, N>` is a fixed-capacity ring buffer with async
//! `send` (parks when full) and async `recv` (parks when empty).
//! Cheaply cloneable — internally an `Rc<RefCell<...>>` so multiple
//! handles can share ownership across spawned tasks.
//!
//! ```ignore
//! use akiva_sdk::sync::Channel;
//! use std::rc::Rc;
//!
//! let ch: Rc<Channel<i32, 4>> = Rc::new(Channel::new());
//!
//! let producer = Rc::clone(&ch);
//! ctx.activity_scope().spawn(async move {
//!     for i in 0..10 {
//!         producer.send(i).await;
//!     }
//! });
//!
//! let consumer = Rc::clone(&ch);
//! ctx.activity_scope().spawn(async move {
//!     for _ in 0..10 {
//!         let v = consumer.recv().await;
//!         akiva_sdk::log::info!("got {v}");
//!     }
//! });
//! ```

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::task::{Poll, Waker};

/// Bounded async channel. `T` is the element type; `N` is the buffer
/// capacity in slots. Cap is encoded as a const generic so the buffer
/// size is part of the type and can't drift between sender and
/// receiver halves.
pub struct Channel<T, const N: usize> {
    inner: RefCell<ChannelInner<T>>,
}

struct ChannelInner<T> {
    buf: VecDeque<T>,
    /// Wakers to notify when at least one slot frees (for parked
    /// senders). Wake-all on each free so a sender that's been
    /// dropped doesn't block the queue.
    sender_wakers: Vec<Waker>,
    /// Wakers to notify when at least one value lands (for parked
    /// receivers). Same wake-all strategy.
    receiver_wakers: Vec<Waker>,
}

/// Returned by [`Channel::try_send`] when the buffer is full. Holds
/// the value the caller tried to send so it isn't dropped silently.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T, const N: usize> Channel<T, N> {
    /// Construct an empty channel with capacity `N`. Panics in const
    /// context if `N == 0` would be allowed; runtime check too because
    /// `assert!` on const-generic isn't yet stable.
    pub fn new() -> Self {
        assert!(N > 0, "Channel<T, 0> is not allowed — capacity must be positive");
        Self {
            inner: RefCell::new(ChannelInner {
                buf: VecDeque::with_capacity(N),
                sender_wakers: Vec::new(),
                receiver_wakers: Vec::new(),
            }),
        }
    }

    /// Number of values currently buffered.
    pub fn len(&self) -> usize {
        self.inner.borrow().buf.len()
    }

    /// True iff [`Channel::len`] is zero.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Capacity passed as `N`. Convenience for symmetry with `len`.
    pub fn capacity(&self) -> usize {
        N
    }

    /// Synchronous push. Returns `Err(SendError(value))` if the
    /// buffer is full so the caller can either back off or fall back
    /// to the async [`Channel::send`].
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        let mut inner = self.inner.borrow_mut();
        if inner.buf.len() >= N {
            return Err(SendError(value));
        }
        inner.buf.push_back(value);
        wake_all(&mut inner.receiver_wakers);
        Ok(())
    }

    /// Synchronous pop. Returns `None` if the buffer is empty.
    pub fn try_recv(&self) -> Option<T> {
        let mut inner = self.inner.borrow_mut();
        let value = inner.buf.pop_front();
        if value.is_some() {
            wake_all(&mut inner.sender_wakers);
        }
        value
    }

    /// Async push. If the buffer is full, the future parks until a
    /// receiver pops a value (or the channel is dropped). Resolves
    /// to `()` once the value has been enqueued.
    pub fn send(&self, value: T) -> impl std::future::Future<Output = ()> + '_ {
        let mut value_holder = Some(value);
        poll_fn(move |cx| {
            let mut inner = self.inner.borrow_mut();
            if inner.buf.len() >= N {
                inner.sender_wakers.push(cx.waker().clone());
                return Poll::Pending;
            }
            // Take and push. `expect` fires only if a future is polled
            // after Ready, which would be a runtime contract violation.
            let value = value_holder.take().expect("send polled after completion");
            inner.buf.push_back(value);
            wake_all(&mut inner.receiver_wakers);
            Poll::Ready(())
        })
    }

    /// Async pop. If the buffer is empty, the future parks until a
    /// sender pushes a value. Resolves to the popped element.
    pub fn recv(&self) -> impl std::future::Future<Output = T> + '_ {
        poll_fn(move |cx| {
            let mut inner = self.inner.borrow_mut();
            match inner.buf.pop_front() {
                Some(v) => {
                    wake_all(&mut inner.sender_wakers);
                    Poll::Ready(v)
                }
                None => {
                    inner.receiver_wakers.push(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
    }
}

impl<T, const N: usize> Default for Channel<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

fn wake_all(wakers: &mut Vec<Waker>) {
    // Drain into a temp so re-polling tasks that re-register wakers
    // don't re-wake themselves in the same pass.
    for w in std::mem::take(wakers) {
        w.wake();
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
    fn try_send_try_recv_round_trip() {
        let ch: Channel<i32, 4> = Channel::new();
        ch.try_send(1).unwrap();
        ch.try_send(2).unwrap();
        ch.try_send(3).unwrap();
        ch.try_send(4).unwrap();
        assert_eq!(ch.try_send(5), Err(SendError(5)));
        assert_eq!(ch.try_recv(), Some(1));
        assert_eq!(ch.try_recv(), Some(2));
        ch.try_send(5).unwrap();
        assert_eq!(ch.try_recv(), Some(3));
        assert_eq!(ch.try_recv(), Some(4));
        assert_eq!(ch.try_recv(), Some(5));
        assert_eq!(ch.try_recv(), None);
    }

    #[test]
    fn capacity_zero_panics() {
        let result = std::panic::catch_unwind(|| {
            let _ch: Channel<i32, 0> = Channel::new();
        });
        assert!(result.is_err());
    }

    #[test]
    fn async_send_recv_ten_items_in_order() {
        // Producer + consumer pattern with backpressure (capacity 4,
        // 10 items). Drive both via the executor's wake flag.
        let ch: Rc<Channel<i32, 4>> = Rc::new(Channel::new());
        let received: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));

        let producer = Rc::clone(&ch);
        let mut producer_fut: BoxFut<()> = Box::pin(async move {
            for i in 0..10 {
                producer.send(i).await;
            }
        });
        let consumer_ch = Rc::clone(&ch);
        let consumer_received = Rc::clone(&received);
        let mut consumer_fut: BoxFut<()> = Box::pin(async move {
            for _ in 0..10 {
                let v = consumer_ch.recv().await;
                consumer_received.borrow_mut().push(v);
            }
        });

        drive_two(&mut producer_fut, &mut consumer_fut);

        assert_eq!(*received.borrow(), (0..10).collect::<Vec<i32>>());
        assert!(ch.is_empty());
    }

    #[test]
    fn backpressure_send_blocks_when_full_and_releases_on_recv() {
        let ch: Rc<Channel<i32, 2>> = Rc::new(Channel::new());
        ch.try_send(1).unwrap();
        ch.try_send(2).unwrap();

        let send_ch = Rc::clone(&ch);
        let mut send_fut: BoxFut<()> = Box::pin(async move {
            send_ch.send(3).await;
        });
        // First poll: full → Pending, registers waker.
        assert!(matches!(poll_once(&mut send_fut), Poll::Pending));
        let _ = take_wake_flag();

        // Pop one value — frees a slot, wakes senders.
        assert_eq!(ch.try_recv(), Some(1));
        assert!(take_wake_flag(), "try_recv should set wake flag for parked sender");

        // Re-poll send — now succeeds.
        assert!(matches!(poll_once(&mut send_fut), Poll::Ready(())));
        assert_eq!(ch.try_recv(), Some(2));
        assert_eq!(ch.try_recv(), Some(3));
        assert_eq!(ch.try_recv(), None);
    }

    #[test]
    fn recv_blocks_when_empty_and_wakes_on_send() {
        let ch: Rc<Channel<i32, 4>> = Rc::new(Channel::new());
        let recv_ch = Rc::clone(&ch);
        let mut recv_fut: BoxFut<i32> = Box::pin(async move { recv_ch.recv().await });
        // First poll: empty → Pending.
        assert!(matches!(poll_once(&mut recv_fut), Poll::Pending));
        let _ = take_wake_flag();

        ch.try_send(42).unwrap();
        assert!(take_wake_flag(), "try_send should set wake flag for parked receiver");

        match poll_once(&mut recv_fut) {
            Poll::Ready(v) => assert_eq!(v, 42),
            Poll::Pending => panic!("expected Ready after sender pushed a value"),
        }
    }

    /// Drive two futures alternately until both resolve, mirroring
    /// `drive_spawned`'s wake-flag loop without pulling in slot
    /// machinery. Bounded so a deadlock surfaces as a test failure.
    fn drive_two(a: &mut BoxFut<()>, b: &mut BoxFut<()>) {
        let mut a_done = false;
        let mut b_done = false;
        for _ in 0..1024 {
            if !a_done {
                if let Poll::Ready(()) = poll_once(a) {
                    a_done = true;
                }
            }
            if !b_done {
                if let Poll::Ready(()) = poll_once(b) {
                    b_done = true;
                }
            }
            if a_done && b_done { return; }
            // Clear the wake flag. If no one woke and we're not done,
            // keep looping — the producer wakes the consumer (and
            // vice versa) on each cycle, so the flag should stay set
            // until everyone resolves.
            let _ = take_wake_flag();
        }
        panic!("drive_two: futures didn't complete in 1024 iterations (deadlock?)");
    }
}
