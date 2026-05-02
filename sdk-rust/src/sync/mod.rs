//! Async coordination primitives for spawned tasks (Stone 11.6.8 #3).
//!
//! Stone 11.6.5 #11 shipped [`crate::activity::TaskScope::spawn`] — a
//! lifecycle-scoped way to launch concurrent tasks. But two spawned
//! tasks have no built-in way to coordinate: there's no `Channel<T>`
//! to push values from a producer task to a consumer task, no
//! `Mutex<T>` to protect shared state, no `Signal<T>` for "wake every
//! observer when this changes." Kotlin Coroutines ship those primitives
//! out of the box; SwiftUI ships `AsyncStream`, `actor`, `@MainActor`.
//! Without them, an Akiva app that wants a producer-consumer pattern
//! has to build it from `RefCell<VecDeque<T>>` plus manual waker
//! plumbing.
//!
//! This module ships the missing pieces, custom-fitted to the
//! single-threaded wasm runtime:
//!
//! - [`Channel<T, N>`] — bounded MPSC ring buffer with async
//!   backpressure on `send` and async waiting on `recv`.
//! - [`Mutex<T>`] — futurized exclusive lock with `MutexGuard` RAII.
//! - [`Signal<T>`] — single-slot, multi-waiter notification (mirror
//!   of Compose's `mutableStateOf` for change-driven UI updates).
//!
//! ## Why custom, not embassy-sync
//!
//! `embassy-sync` is excellent but designed for embedded targets with
//! cross-thread atomics. Wasm in a worker is single-threaded and the
//! whole executor runs on one thread; atomic ops aren't necessary,
//! and the embassy footprint (~3-4 KiB compiled per primitive used)
//! isn't justified for ~100 lines of single-threaded ring-buffer
//! code. The full set here is ~250 lines and integrates directly with
//! `crate::executor`'s wake-flag mechanism.
//!
//! ## Wake propagation
//!
//! Every `.wake()` call on a Waker handed out by these primitives
//! sets the executor's thread-local wake flag (see
//! [`crate::executor::take_wake_flag`]). [`crate::runtime::drive_spawned`]
//! re-polls all spawned tasks until no further wake fires — that's
//! what makes "send → recv" coordination work without a real event
//! loop. Concretely: producer spawns, blocks on send when full;
//! consumer spawns, blocks on recv when empty; producer wakes when
//! consumer pulls a value (slot frees), consumer wakes when producer
//! pushes one. The drive-loop keeps re-polling both until quiescence.
//!
//! ## Limitation: callback dispatches
//!
//! Callback dispatches (e.g. `on_create` awaiting on a Channel.recv())
//! are NOT re-polled by `drive_spawned` — they live in `slot.pending`,
//! polled by `poll_dispatch` only on `host_op_done` arrival. A
//! callback that awaits purely on internal sync primitives (no host
//! op in the chain) may stall. The intended pattern is for
//! `TaskScope::spawn`'d tasks to use these primitives; callbacks
//! should `.await` host operations directly.

pub mod channel;
pub mod mutex;
pub mod signal;

pub use channel::{Channel, SendError};
pub use mutex::{Mutex, MutexGuard};
pub use signal::Signal;
