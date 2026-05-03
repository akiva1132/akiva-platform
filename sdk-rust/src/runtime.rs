//! App-side runtime: handle table + ABI-v0 glue.
//!
//! Hidden implementation behind the `akiva_app!` macro. App authors never
//! call into this module directly — the macro generates `#[no_mangle]`
//! `extern "C"` exports that funnel through `on_*` helpers here, which
//! then dispatch to the user's `Activity` impl.
//!
//! ## Dispatch protocol (P0 #4)
//!
//! Three exports cover every host→app interaction:
//!
//! 1. `akiva_app_dispatch(dispatch_id, cmd_kind, payload_ptr, payload_len) -> u32`
//!    Accept the dispatch, run it as far as possible synchronously, and
//!    return `0` on success or a non-zero status code on a malformed
//!    payload / unknown cmd. The host always allocates `dispatch_id` and
//!    holds it until the bytes (if any) are pulled via `get_result`.
//!
//! 2. `akiva_app_poll_dispatch(dispatch_id) -> u64` — packed
//!    `(status, len)`. Re-drives the parked future once and reports
//!    where it stands: `PENDING` (still in flight, host should call
//!    again after a `host_op_done` arrives), `READY` (bytes available
//!    via `get_result`), `ERROR` (bad input or future panicked).
//!
//! 3. `akiva_app_get_result(dispatch_id, out_len_ptr) -> *const u8`
//!    Two-phase fetch (pointer + length) of the result bytes. Empty
//!    return for ack-only dispatches (lifecycle, input). The buffer
//!    stays valid until the dispatch is consumed (a fitting copy in
//!    `get_result` evicts the entry).
//!
//! The new shape lets app code `.await` on cross-worker host operations
//! (`storage::read`, future `query_packages`, network) without panicking
//! — when the inner future returns `Pending`, the dispatch parks; when
//! the host posts `host_op_done` back, the worker shell calls
//! `poll_dispatch` to resume. See `executor` for the soundness argument
//! around the self-referential future storage.
//!
//! Render and `save_state` keep the synchronous fast-path: the legacy
//! `executor::run_to_ready` panics on `Pending` so accidental awaits in
//! paint code surface immediately. Async storage reads belong in
//! `on_create` / `on_resume`, not `on_render` (render fires every
//! frame; paint code shouldn't block on storage).

#![doc(hidden)]

use std::alloc::{alloc as std_alloc, dealloc as std_dealloc, Layout};
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use crate::activity::{
    Activity, Context, KeyEvent, KeyPhase, Metrics, PointerEvent, PointerPhase, SavedState,
    ScopeKind,
};
use crate::executor;
use crate::intent::Intent;
use crate::op_kinds;
use crate::result::ActivityResult;

// Host-provided imports. Matches `AppRegistry._buildAppImports` in
// runtime/web/app_registry.js. Gated on wasm32 so non-wasm builds
// (cargo test) link without needing to provide the symbols — the
// corresponding helper functions below fall back to in-process
// approximations when the host isn't available.
#[cfg(target_arch = "wasm32")]
extern "C" {
    pub fn akiva_host_fire(op_kind: u32, args_ptr: *const u8, args_len: u32) -> u32;
    pub fn akiva_host_io_start(op_kind: u32, args_ptr: *const u8, args_len: u32) -> u64;
    pub fn akiva_host_io_poll(future_id: u64, out_ptr: *mut u8, out_cap: u32) -> u64;
    /// Stone 11.6.8 #1 — fill `len` bytes at `out_ptr` with
    /// cryptographically secure random data. Maps to
    /// `crypto.getRandomValues` in the JS host (chunked internally
    /// around the 64 KiB Web Crypto cap). Returns `0` on success;
    /// non-zero on failure (in practice never fails). Ungated — RNG
    /// is a primitive, not a capability.
    ///
    /// This signature matches what `getrandom`'s custom-backend
    /// contract expects of a system entropy source. `crate::random`
    /// wires it via `register_custom_getrandom!` so every Rust crate
    /// in the ecosystem (rand, uuid, ring, rustls, …) works in Akiva
    /// apps without app-side glue.
    pub fn akiva_host_random_bytes(out_ptr: *mut u8, len: usize) -> u32;
    /// Stone 11.6.8 #4 — millisecond reading for the requested
    /// clock. Mirrors POSIX `clock_gettime(clockid_t, ...)`.
    ///
    /// `clock_id`:
    /// - `0` = `Monotonic` — `performance.now()`. Never jumps
    ///   backward; reference epoch is unspecified (process start in
    ///   practice). Use for elapsed-time, timeouts.
    /// - `1` = `RealTime`  — `Date.now()`. Wall-clock time, ms since
    ///   UNIX epoch. May jump (NTP, DST, manual clock change). Use
    ///   for user-visible "created at" timestamps.
    ///
    /// Single extern with id keeps the ABI extensible — future clock
    /// types (`ProcessCputimeId`, etc.) won't need a fresh extern
    /// or an ABI bump for the addition.
    pub fn akiva_host_clock_ms(clock_id: u32) -> u64;
}

// ── Side-effect raisers (Stone 9.x; unchanged across P0 #4) ────────────

pub fn host_finish(handle: u32) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = host_fire(op_kinds::fire::FINISH, &handle.to_le_bytes());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = handle;
    }
}

pub fn host_start_activity(handle: u32, intent: &Intent) {
    let bytes = intent.encode();
    #[cfg(target_arch = "wasm32")]
    {
        let mut args = Vec::with_capacity(4 + bytes.len());
        args.extend_from_slice(&handle.to_le_bytes());
        args.extend_from_slice(&bytes);
        let _ = host_fire(op_kinds::fire::START_ACTIVITY, &args);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (handle, bytes);
    }
}

pub fn host_start_activity_for_result(handle: u32, intent: &Intent, request_code: u32) {
    let bytes = intent.encode();
    #[cfg(target_arch = "wasm32")]
    {
        let mut args = Vec::with_capacity(8 + bytes.len());
        args.extend_from_slice(&handle.to_le_bytes());
        args.extend_from_slice(&request_code.to_le_bytes());
        args.extend_from_slice(&bytes);
        let _ = host_fire(op_kinds::fire::START_ACTIVITY_FOR_RESULT, &args);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (handle, bytes, request_code);
    }
}

pub fn host_set_result(handle: u32, result: &ActivityResult) {
    let bytes = result.encode();
    #[cfg(target_arch = "wasm32")]
    {
        let mut args = Vec::with_capacity(4 + bytes.len());
        args.extend_from_slice(&handle.to_le_bytes());
        args.extend_from_slice(&bytes);
        let _ = host_fire(op_kinds::fire::SET_RESULT, &args);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (handle, bytes);
    }
}

// ── Unified-ABI thin wrappers around the wasm imports ──────────────────

#[cfg(target_arch = "wasm32")]
pub(crate) fn host_fire(op_kind: u32, args: &[u8]) -> u32 {
    unsafe { akiva_host_fire(op_kind, args.as_ptr(), args.len() as u32) }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn host_io_start(op_kind: u32, args: &[u8]) -> u64 {
    unsafe { akiva_host_io_start(op_kind, args.as_ptr(), args.len() as u32) }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn host_io_poll(future_id: u64, out: &mut [u8]) -> u64 {
    unsafe { akiva_host_io_poll(future_id, out.as_mut_ptr(), out.len() as u32) }
}

/// Measure a rendered UTF-8 string's width at `size` pixels.
pub fn text_width(text: &str, size: f32) -> f32 {
    #[cfg(target_arch = "wasm32")]
    {
        let mut args = Vec::with_capacity(4 + text.len());
        args.extend_from_slice(&size.to_le_bytes());
        args.extend_from_slice(text.as_bytes());
        let future_id = host_io_start(op_kinds::io::TEXT_WIDTH, &args);
        if future_id == 0 {
            return text_width_fallback(text, size);
        }
        let mut buf = [0u8; 4];
        let packed = host_io_poll(future_id, &mut buf);
        let (status, len) = op_kinds::unpack_poll(packed);
        if status == op_kinds::status::READY && len == 4 {
            f32::from_le_bytes(buf)
        } else {
            text_width_fallback(text, size)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        text_width_fallback(text, size)
    }
}

#[inline]
fn text_width_fallback(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * 0.55
}

// ── Memory ABI ─────────────────────────────────────────────────────────

pub fn alloc(size: u32) -> *mut u8 {
    if size == 0 { return core::ptr::null_mut(); }
    let layout = match Layout::from_size_align(size as usize, 1) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    unsafe { std_alloc(layout) }
}

pub fn dealloc(ptr: *mut u8, size: u32) {
    if ptr.is_null() || size == 0 { return; }
    let layout = match Layout::from_size_align(size as usize, 1) {
        Ok(l) => l,
        Err(_) => return,
    };
    unsafe { std_dealloc(ptr, layout); }
}

pub const ABI_VERSION: &[u8] = b"0.10.0\0";

/// Stone 11.6.8 #2 — per-slot backpressure cap on concurrent
/// in-flight dispatches.
///
/// `Slot.pending` became a `HashMap<u64, PendingDispatch>` in Stone
/// 11.6.5 #12 to admit `futures::join!(read("a"), read("b"))` patterns;
/// the map has been unbounded since. A hostile or buggy app calling
/// `for _ in 0..1_000_000 { ctx.activity_scope().spawn(slow_io()); }`
/// could allocate a million `dispatch_id`s before the watchdog catches
/// the worker at 1.5 s; memory consumption was unbounded.
///
/// 256 is wide enough for legitimate `join_all` of 100+ items, narrow
/// enough to catch spam loops in <50 ms (one dispatch cycle is ~5-50
/// µs, so 256 dispatches cap memory growth around tens of KiB). The
/// JS-side mirror lives in `runtime/web/app_worker_shell.js` as
/// `MAX_PENDING_FUTURES`; the two MUST stay equal.
///
/// When a slot hits the cap, `install_and_poll` returns
/// `Err(REFUSED_BUSY)`; the host surfaces this as `STATUS_BUSY` over
/// the io_poll channel and the SDK's `StorageError::Busy` covers the
/// app-visible side.
pub const MAX_PENDING_PER_SLOT: usize = 256;

/// Refusal code returned by `dispatch` when the slot already has
/// `MAX_PENDING_PER_SLOT` callbacks in flight. Distinct from `1`
/// (bad payload) and `2` (slot missing) so host logs can disambiguate.
pub const REFUSED_BUSY: u32 = 3;

// ── Class string interning ─────────────────────────────────────────────
//
// Activity callbacks see `Context::class` as `&'static str`. The class
// string the host hands us in `CMD::CREATE` is borrowed from the
// payload buffer the host owns for the lifetime of one dispatch — not
// 'static. To make it safely 'static we intern it: each unique class
// name leaks into a `Box<str>` once, then every subsequent `on_create`
// for the same class reuses the same `&'static str`. Bounded leak —
// an app declares O(activity classes) names total.

thread_local! {
    static CLASS_INTERN: RefCell<HashMap<String, &'static str>> =
        RefCell::new(HashMap::new());
}

fn intern_class(s: &str) -> &'static str {
    CLASS_INTERN.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(&interned) = map.get(s) {
            return interned;
        }
        let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
        map.insert(s.to_string(), leaked);
        leaked
    })
}

// ── Slot + pending dispatch state ──────────────────────────────────────

const INITIAL_PAINT_CAP: usize = 4 * 1024;

/// Per-handle slot. Holds the `Box<dyn Activity>` instance, all
/// `Context` state, and any in-flight async dispatch that needs to be
/// resumed across `host_op_done` arrivals.
///
/// Slots are stored in a `HashMap<u32, Box<Slot>>` so the heap address
/// of every field is stable — pending futures borrow into the slot
/// via raw pointers and rely on this stability across polls.
pub(crate) struct Slot {
    /// Field declaration order is load-bearing for drop order: the
    /// pending futures + spawned tasks drop FIRST (both potentially
    /// hold raw-pointer borrows into `activity` and `ctx`), then
    /// those drop in turn. Reordering would risk a use-after-free on
    /// Drop.
    ///
    /// Stone 11.6.5 (#12) — keyed by `dispatch_id` so multiple
    /// callback dispatches can coexist in principle. The framework's
    /// `PendingTransition` discipline still serialises callbacks per
    /// handle (one `&mut Activity` raw-ptr borrow at a time), so
    /// today the map holds at most one entry — but the surface no
    /// longer hard-codes "one in-flight dispatch" as a structural
    /// invariant. User-spawned tasks live in `spawned` and are
    /// independently driven by [`drive_spawned`].
    pending: HashMap<u64, PendingDispatch>,
    /// Stone 11.6.5 (#11) — user-spawned tasks under `activity_scope`
    /// or `resumed_scope`. Polled by [`drive_spawned`] every host
    /// wake-up; cancelled selectively when the activity hits the
    /// matching lifecycle edge.
    ///
    /// Each `SpawnedTask` carries `'static` future + a `ScopeKind`. We
    /// drop in declaration order, so the futures here drop AFTER
    /// `pending` but BEFORE `activity`/`ctx` — matching the contract
    /// that pending dispatches resolve before the slot tears down.
    spawned: Vec<SpawnedTask>,
    activity: Box<dyn Activity>,
    /// The `'static` lifetime here is a local lie: `class` is a
    /// `&'static str` from `intern_class`, which actually IS 'static.
    /// All other fields contain owned data only. The lifetime is never
    /// inspected by the borrow checker once stored as `'static`.
    ctx: Context<'static>,
    paint_buf: Vec<u8>,
    save_buf: Vec<u8>,
    /// Which buffer the last completed dispatch produced bytes into.
    /// Used by `get_result` to pick between `paint_buf` and `save_buf`.
    last_outcome: DispatchOutcome,
}

/// Stone 11.6.5 (#11) — one user-spawned task tied to a [`ScopeKind`].
/// Lives in `Slot.spawned` until the future resolves on its own or the
/// activity hits the cancellation edge for `kind`.
struct SpawnedTask {
    future: Pin<Box<dyn Future<Output = ()> + 'static>>,
    kind: ScopeKind,
}

/// What kind of result the last (or in-flight) dispatch is producing.
/// Maps to which buffer `get_result` should hand back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DispatchOutcome {
    /// Ack-only — `get_result` returns 0 bytes.
    NoBytes,
    /// Bytes are in `slot.paint_buf` (RENDER).
    Paint,
    /// Bytes are in `slot.save_buf` (SAVE_STATE).
    SavedState,
    /// Slot should be removed after this dispatch finalises (DESTROY).
    Destroy,
}

/// One in-flight async dispatch. Holds the future and the cached
/// result the next `poll_dispatch` should report.
struct PendingDispatch {
    /// Borrows from `slot.activity` and `slot.ctx` via raw pointers
    /// captured in [`build_async_future`]. SAFETY: see `executor`
    /// module docs — the slot is heap-pinned in `SLOTS` and the
    /// runtime never moves or drops the slot while this future is
    /// alive.
    future: Pin<Box<dyn Future<Output = DispatchOutcome> + 'static>>,
    /// Once the future resolves we cache the outcome here and stop
    /// polling. The next `poll_dispatch` call sees `Some(...)` and
    /// returns `READY` without re-driving the future.
    completed: Option<DispatchOutcome>,
    /// Stone 11.6.5 (#11) — recorded so [`drive_pending`] can apply
    /// scope-cancellation hooks when this dispatch resolves
    /// (`PAUSE` drops `Resumed`-scoped spawns; `DESTROY` drops the
    /// whole slot, which clears spawns naturally on Drop).
    cmd_kind: u32,
}

// ── Global registries ──────────────────────────────────────────────────

thread_local! {
    static SLOTS: RefCell<HashMap<u32, Box<Slot>>> = RefCell::new(HashMap::new());
    /// `dispatch_id -> handle`. `dispatch_id` is allocated by the host
    /// (worker shell) and passed into `akiva_app_dispatch`; we record
    /// the mapping so `poll_dispatch` and `get_result` can find the
    /// owning slot from just the dispatch_id.
    static DISPATCH_TO_HANDLE: RefCell<HashMap<u64, u32>> = RefCell::new(HashMap::new());
    /// Stone 11.6.5 (#11) — staging area for `TaskScope::spawn` calls
    /// made from inside a future poll. Appending directly into
    /// `slot.spawned` would re-borrow `SLOTS` while we're already
    /// holding it for the dispatch poll; this queue absorbs the
    /// spawn, and [`drain_spawn_queue`] flushes it into the right
    /// slot once the outer borrow has released.
    static SPAWN_QUEUE: RefCell<Vec<(u32, ScopeKind, Pin<Box<dyn Future<Output = ()> + 'static>>)>> =
        RefCell::new(Vec::new());
}

fn with_slots<R>(f: impl FnOnce(&mut HashMap<u32, Box<Slot>>) -> R) -> R {
    SLOTS.with(|cell| f(&mut cell.borrow_mut()))
}

fn with_dispatches<R>(f: impl FnOnce(&mut HashMap<u64, u32>) -> R) -> R {
    DISPATCH_TO_HANDLE.with(|cell| f(&mut cell.borrow_mut()))
}

/// Stone 11.6.5 (#11) — push a future into the staging queue. Called
/// from `TaskScope::spawn`, possibly while we're inside a dispatch
/// poll holding a `SLOTS` borrow.
pub(crate) fn spawn_in_slot(
    handle: u32,
    kind: ScopeKind,
    future: Pin<Box<dyn Future<Output = ()> + 'static>>,
) {
    SPAWN_QUEUE.with(|q| q.borrow_mut().push((handle, kind, future)));
}

/// Move every staged spawn from `SPAWN_QUEUE` into its owning slot's
/// `spawned` vec. Called after every dispatch poll and at the start
/// of [`drive_spawned`] so apps can spawn from inside any callback or
/// any spawned task.
fn drain_spawn_queue() {
    let drained: Vec<_> = SPAWN_QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if q.is_empty() {
            return Vec::new();
        }
        std::mem::take(&mut *q)
    });
    if drained.is_empty() {
        return;
    }
    with_slots(|slots| {
        for (handle, kind, future) in drained {
            if let Some(slot) = slots.get_mut(&handle) {
                slot.spawned.push(SpawnedTask { future, kind });
            }
            // Slot evicted before drain → drop the future. App that
            // spawns from inside on_destroy and gets here after slot
            // teardown should silently no-op rather than panic.
        }
    });
}

/// Stone 11.6.5 (#11) — poll every pending spawned task across every
/// live slot. Tasks that resolve drop; pending tasks stay armed for
/// the next sweep. Called by `akiva_app_drive_spawned` from the host
/// worker shell once per `host_op_done` arrival (and once per dispatch
/// resolution); apps don't see this directly.
///
/// Returns the count of tasks still pending after the sweep — useful
/// for the host to back off polling when everything's idle.
pub fn drive_spawned() -> u32 {
    // Stone 11.6.8 #3 — re-poll until quiescence, not just once.
    //
    // Internal coordination via `crate::sync::{Channel, Mutex,
    // Signal}` calls `Waker::wake()` to signal "needs another poll."
    // Without an event loop, `wake` is a thread-local flag
    // (`executor::take_wake_flag`) — we sweep tasks repeatedly until
    // no future signals progress. Bounded so a misbehaving future
    // that wakes itself every poll can't infinite-loop us.
    const MAX_DRIVE_ITERATIONS: u32 = 64;
    drain_spawn_queue();
    // Clear any stale wake flag from before this drive call so the
    // first iteration's flag-check accurately reflects only this
    // pass's wakes.
    let _ = executor::take_wake_flag();
    let mut total_remaining: u32 = 0;
    for _ in 0..MAX_DRIVE_ITERATIONS {
        total_remaining = 0;
        with_slots(|slots| {
            for slot in slots.values_mut() {
                slot.spawned.retain_mut(|task| {
                    match executor::poll_once(&mut task.future) {
                        Poll::Ready(()) => false,
                        Poll::Pending => true,
                    }
                });
                total_remaining += slot.spawned.len() as u32;
            }
        });
        // Tasks may have spawned siblings during their poll — drain
        // before checking the wake flag so freshly-staged tasks get
        // a chance on the next iteration.
        drain_spawn_queue();
        if !executor::take_wake_flag() {
            break;
        }
    }
    total_remaining
}

// ── Future construction ───────────────────────────────────────────────
//
// `build_async_future` hands back a `Pin<Box<dyn Future<Output =
// DispatchOutcome> + 'static>>` that internally borrows the slot's
// `activity` and `ctx` fields via raw pointers. The async-trait
// expansion of `Activity::on_*` produces a `Pin<Box<dyn Future<Output
// = ()> + 'a + 'b>>` where the lifetimes flow from `&mut self` and
// `&mut ctx`. By feeding it `'static` references (faked via raw
// pointer cast), the resulting future is `'static` — at which point
// we can store it in the slot.
//
// SAFETY summary: the runtime never mutates `slot.activity` or
// `slot.ctx` while a `PendingDispatch` exists; the framework's
// `PendingTransition` discipline guarantees one in-flight dispatch
// per handle. The slot itself stays in `SLOTS` (heap-stable) until
// the pending future resolves to `Destroy`, at which point we drop
// the future first and only then remove the slot.

unsafe fn raw_static_mut<T: ?Sized>(p: *mut T) -> &'static mut T {
    // SAFETY: caller guarantees `p` points at a heap-stable allocation
    // that won't move or drop while the resulting reference is alive
    // (typically: until the storing future is dropped).
    &mut *(p as *mut T)
}

fn build_async_future<F>(
    slot: &mut Slot,
    outcome: DispatchOutcome,
    body: F,
) -> Pin<Box<dyn Future<Output = DispatchOutcome> + 'static>>
where
    F: FnOnce(
            &'static mut Box<dyn Activity>,
            &'static mut Context<'static>,
        ) -> Pin<Box<dyn Future<Output = ()> + 'static>>
        + 'static,
{
    let activity_ptr: *mut Box<dyn Activity> = &mut slot.activity;
    let ctx_ptr: *mut Context<'static> = &mut slot.ctx;
    Box::pin(async move {
        // SAFETY: see module docs. Pointers come from a heap-pinned
        // slot owned by SLOTS; the framework guarantees no concurrent
        // mutation; the future is dropped before slot teardown.
        let activity = unsafe { raw_static_mut(activity_ptr) };
        let ctx = unsafe { raw_static_mut(ctx_ptr) };
        body(activity, ctx).await;
        outcome
    })
}

// ── Dispatch entry points ──────────────────────────────────────────────

/// Build the future for one cmd_kind, install it as the slot's pending
/// dispatch (replacing any prior — debug_assert there isn't one), and
/// poll it once. Returns the immediate outcome status: ready bytes
/// length, pending, or terminal completion.
fn install_and_poll<T: Activity + Default>(
    handle: u32,
    dispatch_id: u64,
    cmd_kind: u32,
    rest: &[u8],
) -> Result<(), u32> {
    use crate::op_kinds::cmd::*;

    // CREATE is special: it constructs the slot if absent.
    if cmd_kind == CREATE {
        let class_str = core::str::from_utf8(rest).unwrap_or("");
        let class_static = intern_class(class_str);
        let activity: Box<dyn Activity> = Box::new(T::default());
        let mut new_slot = Box::new(Slot {
            pending: HashMap::new(),
            spawned: Vec::new(),
            activity,
            ctx: Context {
                metrics: Metrics::default(),
                timestamp_ms: 0.0,
                pointer: None,
                class: class_static,
                handle,
            },
            paint_buf: Vec::with_capacity(INITIAL_PAINT_CAP),
            save_buf: Vec::new(),
            last_outcome: DispatchOutcome::NoBytes,
        });

        // Build the on_create future.
        let fut = build_async_future(&mut *new_slot, DispatchOutcome::NoBytes, |a, c| {
            Box::pin(async move { a.on_create(c).await; })
        });
        new_slot.pending.insert(dispatch_id, PendingDispatch {
            future: fut,
            completed: None,
            cmd_kind: CREATE,
        });

        // Install slot before polling — poll's raw pointers read from
        // SLOTS-resident memory.
        with_slots(|s| s.insert(handle, new_slot));
        with_dispatches(|d| d.insert(dispatch_id, handle));
        // Initial poll inside its own borrow scope.
        let _ = drive_pending(dispatch_id);
        return Ok(());
    }

    // For non-CREATE cmds the slot must already exist.
    let exists = with_slots(|s| s.contains_key(&handle));
    if !exists {
        return Err(1);
    }
    // Stone 11.6.8 #2 — backpressure: refuse if the slot is at the
    // pending-dispatch cap. The framework normally serializes
    // callback dispatches per handle so this is cold-path defense,
    // but a buggy host injecting many parallel CMDs would otherwise
    // grow `slot.pending` without bound.
    let too_many = with_slots(|s| {
        s.get(&handle).is_some_and(|slot| slot.pending.len() >= MAX_PENDING_PER_SLOT)
    });
    if too_many {
        return Err(REFUSED_BUSY);
    }

    // Build the future inside a borrow-scope, then poll separately
    // (drive_pending takes its own borrow).
    with_slots(|slots_map| {
        let slot = slots_map.get_mut(&handle).expect("checked above");
        // Stone 11.6.5 (#12) — slot.pending is a HashMap<dispatch_id,
        // PendingDispatch>, so a fresh callback gets its own slot in
        // the map keyed by `dispatch_id`. The framework's
        // PendingTransition discipline still serialises callback
        // dispatches per handle; what changed is that the surface no
        // longer requires it as a structural invariant — the map is
        // keyed by the host's dispatch_id, so concurrent entries
        // would coexist if the framework ever permitted them.
        slot.last_outcome = DispatchOutcome::NoBytes;

        match cmd_kind {
            START   => install_lifecycle(slot, dispatch_id, |a, c| Box::pin(async move { a.on_start(c).await; })),
            RESUME  => install_lifecycle(slot, dispatch_id, |a, c| Box::pin(async move { a.on_resume(c).await; })),
            PAUSE   => install_lifecycle(slot, dispatch_id, |a, c| Box::pin(async move { a.on_pause(c).await; })),
            STOP    => install_lifecycle(slot, dispatch_id, |a, c| Box::pin(async move { a.on_stop(c).await; })),
            DESTROY => install_destroy(slot, dispatch_id),

            POINTER => install_pointer(slot, dispatch_id, rest)?,
            KEY     => install_key(slot, dispatch_id, rest)?,
            RESIZE  => install_resize(slot, dispatch_id, rest)?,

            RENDER => {
                if rest.len() < 8 { return Err(1); }
                let ts = read_f64(&rest[..8]);
                slot.ctx.timestamp_ms = ts;
                run_render_sync(slot, dispatch_id);
            }

            SAVE_STATE => run_save_state_sync(slot, dispatch_id),
            RESTORE_STATE => {
                run_restore_state_sync(slot, dispatch_id, rest);
            }

            NEW_INTENT => install_new_intent(slot, dispatch_id, rest)?,
            ACTIVITY_RESULT => install_activity_result(slot, dispatch_id, rest)?,

            _ => return Err(1),
        }
        // Stone 11.6.5 (#11) — stamp cmd_kind on the freshly installed
        // pending dispatch so `drive_pending` can apply scope-cancel
        // hooks (PAUSE/DESTROY) when the future resolves. The various
        // install_* helpers default to 0; we patch the truth post-install
        // here.
        if let Some(p) = slot.pending.get_mut(&dispatch_id) {
            p.cmd_kind = cmd_kind;
        }
        Ok::<(), u32>(())
    })?;

    // Map dispatch_id → handle, then drive the pending (or skip if
    // sync-completed).
    with_dispatches(|d| d.insert(dispatch_id, handle));
    let _ = drive_pending(dispatch_id);
    Ok(())
}

/// Install a lifecycle future (Start/Resume/Pause/Stop). The body runs
/// the corresponding `Activity::on_*(&mut self, &mut ctx)` async fn.
fn install_lifecycle<F>(slot: &mut Slot, dispatch_id: u64, body: F)
where
    F: FnOnce(
            &'static mut Box<dyn Activity>,
            &'static mut Context<'static>,
        ) -> Pin<Box<dyn Future<Output = ()> + 'static>>
        + 'static,
{
    let fut = build_async_future(slot, DispatchOutcome::NoBytes, body);
    slot.pending.insert(dispatch_id, PendingDispatch { future: fut, completed: None, cmd_kind: 0 });
}

fn install_destroy(slot: &mut Slot, dispatch_id: u64) {
    let fut = build_async_future(slot, DispatchOutcome::Destroy, |a, c| {
        Box::pin(async move { a.on_destroy(c).await; })
    });
    slot.pending.insert(dispatch_id, PendingDispatch { future: fut, completed: None, cmd_kind: 0 });
}

fn install_pointer(slot: &mut Slot, dispatch_id: u64, rest: &[u8]) -> Result<(), u32> {
    if rest.len() < 24 { return Err(1); }
    let x = f32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let y = f32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
    let button = u32::from_le_bytes([rest[8], rest[9], rest[10], rest[11]]);
    let phase = u32::from_le_bytes([rest[12], rest[13], rest[14], rest[15]]);
    let ts = read_f64(&rest[16..24]);

    let ev = PointerEvent {
        x, y,
        button: button as u8,
        phase: decode_pointer_phase(phase),
    };
    slot.ctx.pointer = Some(ev);
    slot.ctx.timestamp_ms = ts;

    let fut = build_async_future(slot, DispatchOutcome::NoBytes, move |a, c| {
        Box::pin(async move { a.on_pointer(c, ev).await; })
    });
    slot.pending.insert(dispatch_id, PendingDispatch { future: fut, completed: None, cmd_kind: 0 });
    Ok(())
}

fn install_key(slot: &mut Slot, dispatch_id: u64, rest: &[u8]) -> Result<(), u32> {
    if rest.len() < 20 { return Err(1); }
    let code = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let modifiers = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
    let phase = u32::from_le_bytes([rest[8], rest[9], rest[10], rest[11]]);
    let ts = read_f64(&rest[12..20]);

    let ev = KeyEvent {
        code,
        modifiers: modifiers as u8,
        phase: decode_key_phase(phase),
    };
    slot.ctx.timestamp_ms = ts;

    let fut = build_async_future(slot, DispatchOutcome::NoBytes, move |a, c| {
        Box::pin(async move { a.on_key(c, ev).await; })
    });
    slot.pending.insert(dispatch_id, PendingDispatch { future: fut, completed: None, cmd_kind: 0 });
    Ok(())
}

fn install_resize(slot: &mut Slot, dispatch_id: u64, rest: &[u8]) -> Result<(), u32> {
    if rest.len() < 12 { return Err(1); }
    let w = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let h = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
    let d = f32::from_le_bytes([rest[8], rest[9], rest[10], rest[11]]);
    let metrics = Metrics { width_px: w, height_px: h, density: d };
    slot.ctx.metrics = metrics;

    let fut = build_async_future(slot, DispatchOutcome::NoBytes, move |a, c| {
        Box::pin(async move { a.on_resize(c, metrics).await; })
    });
    slot.pending.insert(dispatch_id, PendingDispatch { future: fut, completed: None, cmd_kind: 0 });
    Ok(())
}

fn install_new_intent(slot: &mut Slot, dispatch_id: u64, rest: &[u8]) -> Result<(), u32> {
    let intent = match Intent::decode(rest) {
        Ok(i) => i,
        Err(_) => {
            // Drop silently — same policy as the legacy code path.
            slot.pending.insert(dispatch_id, PendingDispatch {
                future: Box::pin(async move { DispatchOutcome::NoBytes }),
                completed: None, cmd_kind: 0,
            });
            return Ok(());
        }
    };
    let fut = build_async_future(slot, DispatchOutcome::NoBytes, move |a, c| {
        Box::pin(async move { a.on_new_intent(c, &intent).await; })
    });
    slot.pending.insert(dispatch_id, PendingDispatch { future: fut, completed: None, cmd_kind: 0 });
    Ok(())
}

fn install_activity_result(slot: &mut Slot, dispatch_id: u64, rest: &[u8]) -> Result<(), u32> {
    if rest.len() < 4 { return Err(1); }
    let rc = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let result = match ActivityResult::decode(&rest[4..]) {
        Ok(r) => r,
        Err(_) => {
            slot.pending.insert(dispatch_id, PendingDispatch {
                future: Box::pin(async move { DispatchOutcome::NoBytes }),
                completed: None, cmd_kind: 0,
            });
            return Ok(());
        }
    };
    let fut = build_async_future(slot, DispatchOutcome::NoBytes, move |a, c| {
        Box::pin(async move { a.on_activity_result(c, rc, &result).await; })
    });
    slot.pending.insert(dispatch_id, PendingDispatch { future: fut, completed: None, cmd_kind: 0 });
    Ok(())
}

/// RENDER is sync-only by design: paint code runs every frame and
/// shouldn't block on host I/O. The legacy `executor::run_to_ready`
/// panics on Pending so an accidental `.await` in `on_render` surfaces
/// loudly. Caches the produced bytes in `slot.paint_buf` and marks the
/// slot's outcome.
fn run_render_sync(slot: &mut Slot, dispatch_id: u64) {
    slot.paint_buf.clear();
    slot.last_outcome = DispatchOutcome::Paint;
    // SAFETY: render's future borrows &mut activity, &mut ctx, and
    // &mut paint_buf. We construct all three as &'static via raw
    // pointer casts, since `executor::run_to_ready` consumes the
    // future before this fn returns — the 'static lifetime is a
    // local fiction with no escape.
    let activity_ptr: *mut Box<dyn Activity> = &mut slot.activity;
    let ctx_ptr: *mut Context<'static> = &mut slot.ctx;
    let paint_ptr: *mut Vec<u8> = &mut slot.paint_buf;
    let activity = unsafe { raw_static_mut(activity_ptr) };
    let ctx = unsafe { raw_static_mut(ctx_ptr) };
    let paint = unsafe { raw_static_mut(paint_ptr) };
    let mut canvas = crate::canvas::Canvas::new(paint);
    executor::run_to_ready(activity.on_render(ctx, &mut canvas));
    // Stash a sync-completed marker in the pending slot so
    // poll_dispatch reports READY without trying to re-poll a future
    // that doesn't exist.
    slot.pending.insert(dispatch_id, PendingDispatch {
        future: Box::pin(async move { DispatchOutcome::Paint }),
        completed: Some(DispatchOutcome::Paint), cmd_kind: 0,
    });
}

fn run_save_state_sync(slot: &mut Slot, dispatch_id: u64) {
    let mut state = SavedState::default();
    slot.activity.on_save_state(&mut state);
    slot.save_buf = state.encode();
    slot.last_outcome = DispatchOutcome::SavedState;
    slot.pending.insert(dispatch_id, PendingDispatch {
        future: Box::pin(async move { DispatchOutcome::SavedState }),
        completed: Some(DispatchOutcome::SavedState), cmd_kind: 0,
    });
}

fn run_restore_state_sync(slot: &mut Slot, dispatch_id: u64, rest: &[u8]) {
    if !rest.is_empty() {
        if let Ok(state) = SavedState::decode(rest) {
            slot.activity.on_restore_state(&state);
        }
    }
    slot.last_outcome = DispatchOutcome::NoBytes;
    slot.pending.insert(dispatch_id, PendingDispatch {
        future: Box::pin(async move { DispatchOutcome::NoBytes }),
        completed: Some(DispatchOutcome::NoBytes), cmd_kind: 0,
    });
}

/// Poll the pending future for `dispatch_id` once. Updates
/// `pending.completed` if it reaches a terminal state.
///
/// Stone 11.6.5 (#11) — when a `PAUSE` dispatch reaches `Ready`, every
/// `Resumed`-scoped spawned task on the same slot is dropped. `DESTROY`
/// is handled implicitly: the slot itself evicts in `get_result` and
/// the entire `spawned` Vec drops with it. After driving, any spawns
/// the body queued get flushed into their slots.
fn drive_pending(dispatch_id: u64) -> u32 {
    use crate::op_kinds::cmd;

    let handle = match with_dispatches(|d| d.get(&dispatch_id).copied()) {
        Some(h) => h,
        None => return op_kinds::status::ERROR,
    };
    let status = with_slots(|slots_map| {
        let slot = match slots_map.get_mut(&handle) {
            Some(s) => s,
            None => return op_kinds::status::ERROR,
        };
        let pending = match slot.pending.get_mut(&dispatch_id) {
            Some(p) => p,
            None => return op_kinds::status::ERROR,
        };
        if pending.completed.is_some() {
            return op_kinds::status::READY;
        }
        match executor::poll_once(&mut pending.future) {
            Poll::Pending => op_kinds::status::PENDING,
            Poll::Ready(outcome) => {
                pending.completed = Some(outcome);
                let cmd_kind = pending.cmd_kind;
                slot.last_outcome = outcome;
                // Cancellation hook: drop scoped spawns on the right
                // lifecycle edge. PAUSE drops Resumed-scoped only;
                // DESTROY drops everything when the slot evicts in
                // get_result, so we don't touch spawned here for it.
                if cmd_kind == cmd::PAUSE {
                    slot.spawned.retain(|t| t.kind != ScopeKind::Resumed);
                }
                op_kinds::status::READY
            }
        }
    });
    // Body may have spawned tasks during this poll. Flush them into
    // their slots before returning so a subsequent poll sees them.
    drain_spawn_queue();
    status
}

/// `akiva_app_dispatch` body. Returns `0` on accept (the host should
/// poll via `poll_dispatch` to drive completion); non-zero on a hard
/// refusal (malformed payload, unknown cmd_kind, out-of-memory, etc.).
pub fn dispatch<T: Activity + Default>(
    dispatch_id: u64,
    cmd_kind: u32,
    payload_ptr: *const u8,
    payload_len: u32,
) -> u32 {
    if payload_len < 4 || payload_ptr.is_null() {
        return 1;
    }
    // SAFETY: the host owns the buffer for the duration of this call.
    let payload = unsafe {
        core::slice::from_raw_parts(payload_ptr, payload_len as usize)
    };
    let handle = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let rest = &payload[4..];

    match install_and_poll::<T>(handle, dispatch_id, cmd_kind, rest) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

/// `akiva_app_poll_dispatch` body. Returns `(status, len)` packed:
/// status in low 32 bits, result-bytes length in high 32 bits.
/// Idempotent — terminal states cache and report consistently.
pub fn poll_dispatch(dispatch_id: u64) -> u64 {
    let status = drive_pending(dispatch_id);
    if status != op_kinds::status::READY {
        return pack_poll(status, 0);
    }
    let len = result_bytes_len(dispatch_id);
    pack_poll(status, len)
}

/// `akiva_app_get_result` body. Two-phase fetch: returns the source
/// pointer + writes the byte length to `*out_len_ptr`. Buffer stays
/// valid until the dispatch is consumed by a fitting `get_result`
/// (which evicts the dispatch_id mapping).
///
/// For a `Destroy` outcome, evicts the slot itself once the bytes are
/// drained (always 0 bytes — destroy is ack-only). The future-drop
/// happens here, BEFORE slot eviction, so any raw-pointer borrows the
/// future held into the slot are released first.
pub fn get_result(dispatch_id: u64, out_len_ptr: *mut u32) -> *const u8 {
    let handle = match with_dispatches(|d| d.get(&dispatch_id).copied()) {
        Some(h) => h,
        None => {
            // SAFETY: host always passes a writable u32 cell.
            unsafe { *out_len_ptr = 0; }
            return core::ptr::null();
        }
    };
    let mut destroy_after = false;
    let mut bytes_ptr: *const u8 = core::ptr::null();
    let mut bytes_len: u32 = 0;
    with_slots(|slots_map| {
        let Some(slot) = slots_map.get_mut(&handle) else { return; };
        let outcome = slot.pending.get(&dispatch_id)
            .and_then(|p| p.completed)
            .unwrap_or(slot.last_outcome);
        match outcome {
            DispatchOutcome::Paint => {
                bytes_len = slot.paint_buf.len() as u32;
                bytes_ptr = slot.paint_buf.as_ptr();
            }
            DispatchOutcome::SavedState => {
                bytes_len = slot.save_buf.len() as u32;
                bytes_ptr = slot.save_buf.as_ptr();
            }
            DispatchOutcome::NoBytes => {
                bytes_len = 0;
            }
            DispatchOutcome::Destroy => {
                bytes_len = 0;
                destroy_after = true;
            }
        }
        // Drop the specific dispatch's future BEFORE potentially
        // removing the slot — the future holds raw-pointer borrows
        // into the slot. Other dispatches in the same slot's pending
        // map (Stone 11.6.5 #12) keep running.
        slot.pending.remove(&dispatch_id);
    });

    // Always evict the dispatch_id; single-shot semantic.
    with_dispatches(|d| { d.remove(&dispatch_id); });

    if destroy_after {
        with_slots(|slots_map| { slots_map.remove(&handle); });
    }

    // SAFETY: host always passes a writable u32 cell.
    unsafe { *out_len_ptr = bytes_len; }
    bytes_ptr
}

fn result_bytes_len(dispatch_id: u64) -> u32 {
    let handle = match with_dispatches(|d| d.get(&dispatch_id).copied()) {
        Some(h) => h,
        None => return 0,
    };
    with_slots(|slots_map| {
        let Some(slot) = slots_map.get(&handle) else { return 0; };
        let outcome = slot.pending.get(&dispatch_id)
            .and_then(|p| p.completed)
            .unwrap_or(slot.last_outcome);
        match outcome {
            DispatchOutcome::Paint => slot.paint_buf.len() as u32,
            DispatchOutcome::SavedState => slot.save_buf.len() as u32,
            DispatchOutcome::NoBytes | DispatchOutcome::Destroy => 0,
        }
    })
}

fn pack_poll(status: u32, length: u32) -> u64 {
    (status as u64) | ((length as u64) << 32)
}

// ── Wire decoders ─────────────────────────────────────────────────────

fn decode_pointer_phase(w: u32) -> PointerPhase {
    match w {
        0 => PointerPhase::Down,
        1 => PointerPhase::Move,
        2 => PointerPhase::Up,
        _ => PointerPhase::Cancel,
    }
}

fn decode_key_phase(w: u32) -> KeyPhase {
    match w {
        0 => KeyPhase::Down,
        1 => KeyPhase::Repeat,
        _ => KeyPhase::Up,
    }
}

fn read_f64(bytes: &[u8]) -> f64 {
    debug_assert!(bytes.len() >= 8);
    f64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Activity;
    use crate::op_kinds::cmd;
    use async_trait::async_trait;

    #[derive(Default)]
    struct ImmediateActivity {
        created: bool,
    }

    #[async_trait(?Send)]
    impl Activity for ImmediateActivity {
        async fn on_create(&mut self, _ctx: &mut Context<'_>) {
            self.created = true;
        }
    }

    #[test]
    fn create_then_poll_resolves_synchronously() {
        // P0 #4 — when the user's async fn doesn't suspend, the
        // dispatch should reach READY on the first internal poll
        // (still inside `dispatch`). poll_dispatch reports READY,
        // get_result returns 0 bytes for the ack-only outcome.
        let payload_handle = 1u32;
        let class = "Main";
        let mut payload = Vec::new();
        payload.extend_from_slice(&payload_handle.to_le_bytes());
        payload.extend_from_slice(class.as_bytes());
        let dispatch_id = 100u64;
        let rc = dispatch::<ImmediateActivity>(
            dispatch_id, cmd::CREATE, payload.as_ptr(), payload.len() as u32,
        );
        assert_eq!(rc, 0, "dispatch should accept");
        let packed = poll_dispatch(dispatch_id);
        let status = (packed & 0xFFFF_FFFF) as u32;
        assert_eq!(status, op_kinds::status::READY,
            "immediate-future should resolve synchronously");
        let mut len: u32 = 0;
        let ptr = get_result(dispatch_id, &mut len as *mut u32);
        assert_eq!(len, 0, "create is ack-only");
        let _ = ptr;
        // Cleanup: destroy the slot so subsequent tests see a clean slate.
        let mut destroy_payload = Vec::new();
        destroy_payload.extend_from_slice(&payload_handle.to_le_bytes());
        let did = 101u64;
        dispatch::<ImmediateActivity>(
            did, cmd::DESTROY, destroy_payload.as_ptr(), destroy_payload.len() as u32,
        );
        let _ = poll_dispatch(did);
        let mut zero: u32 = 0;
        get_result(did, &mut zero as *mut u32);
    }

    #[test]
    fn dispatch_rejects_short_payload() {
        let buf = [0u8; 2];
        let rc = dispatch::<ImmediateActivity>(1, cmd::RESUME, buf.as_ptr(), 2);
        assert_ne!(rc, 0);
    }

    #[test]
    fn pack_poll_round_trips() {
        assert_eq!(pack_poll(op_kinds::status::READY, 42), 1u64 | (42u64 << 32));
        assert_eq!(pack_poll(op_kinds::status::PENDING, 0), 0u64);
    }

    /// Activity that does nothing — placeholder for `mem::replace`
    /// dances in `run_lifecycle_with` (a `Slot` field can't be left
    /// uninitialised, so we swap in this dummy when consuming the
    /// real activity).
    #[derive(Default)]
    struct NopActivity;
    #[async_trait(?Send)]
    impl Activity for NopActivity {}

    /// Stone 11.6.5 (#11) — a future that records every poll on a
    /// shared counter and always returns `Pending`. Used to assert
    /// that drive_spawned fires it, and that cancellation stops the
    /// firing.
    struct PollCounterFuture(std::rc::Rc<std::cell::Cell<u32>>);
    impl Future for PollCounterFuture {
        type Output = ();
        fn poll(
            self: Pin<&mut Self>,
            _cx: &mut core::task::Context<'_>,
        ) -> Poll<()> {
            self.0.set(self.0.get() + 1);
            Poll::Pending
        }
    }

    /// Activity that spawns a single counter-future under the given
    /// scope on the matching lifecycle edge. Lifecycle hook is chosen
    /// via `spawn_in_create` / `spawn_in_resume` so tests can target
    /// either side without writing N variants.
    struct SpawnInActivity {
        spawn_in_create: Option<(crate::activity::ScopeKind, std::rc::Rc<std::cell::Cell<u32>>)>,
        spawn_in_resume: Option<(crate::activity::ScopeKind, std::rc::Rc<std::cell::Cell<u32>>)>,
    }
    impl Default for SpawnInActivity {
        fn default() -> Self {
            Self { spawn_in_create: None, spawn_in_resume: None }
        }
    }
    #[async_trait(?Send)]
    impl Activity for SpawnInActivity {
        async fn on_create(&mut self, ctx: &mut Context<'_>) {
            if let Some((kind, counter)) = self.spawn_in_create.take() {
                let scope = match kind {
                    crate::activity::ScopeKind::Activity => ctx.activity_scope(),
                    crate::activity::ScopeKind::Resumed  => ctx.resumed_scope(),
                };
                scope.spawn(PollCounterFuture(counter));
            }
        }
        async fn on_resume(&mut self, ctx: &mut Context<'_>) {
            if let Some((kind, counter)) = self.spawn_in_resume.take() {
                let scope = match kind {
                    crate::activity::ScopeKind::Activity => ctx.activity_scope(),
                    crate::activity::ScopeKind::Resumed  => ctx.resumed_scope(),
                };
                scope.spawn(PollCounterFuture(counter));
            }
        }
    }

    /// Tests need to construct the slot directly (the dispatch path
    /// uses `T::default()` and we want test-owned state). This helper
    /// installs a `Slot` in `SLOTS` for `handle` carrying the supplied
    /// activity, then fires the lifecycle dispatches in order. Returns
    /// nothing — assertions are made via the shared counters captured
    /// in the activity.
    fn run_lifecycle_with(
        handle: u32,
        mut activity: Box<dyn Activity>,
        cmds: &[(u64, u32)],
    ) {
        // Build a Slot with the user's activity. The CREATE path of
        // `dispatch::<T>` would T::default() instead of using ours, so
        // we install manually.
        let class_static = intern_class("Test");
        let mut slot = Box::new(Slot {
            pending: HashMap::new(),
            spawned: Vec::new(),
            activity: std::mem::replace(
                &mut activity,
                Box::new(NopActivity::default()),
            ),
            ctx: Context {
                metrics: Metrics::default(),
                timestamp_ms: 0.0,
                pointer: None,
                class: class_static,
                handle,
            },
            paint_buf: Vec::new(),
            save_buf: Vec::new(),
            last_outcome: DispatchOutcome::NoBytes,
        });
        // Install — pin the slot before any dispatch fires it.
        with_slots(|s| { s.insert(handle, std::mem::replace(&mut slot, Box::new(empty_slot()))); });

        // Run the cmds. CREATE/etc each get a fresh dispatch_id.
        for &(dispatch_id, cmd_kind) in cmds {
            // Build the lifecycle future for the cmd. We bypass
            // install_and_poll because that path constructs a fresh
            // T::default(); we want to drive the existing slot.
            with_slots(|slots| {
                let s = slots.get_mut(&handle).unwrap();
                let body: Box<dyn FnOnce(
                    &'static mut Box<dyn Activity>,
                    &'static mut Context<'static>,
                ) -> Pin<Box<dyn Future<Output = ()> + 'static>> + 'static> = match cmd_kind {
                    cmd::CREATE  => Box::new(|a, c| Box::pin(async move { a.on_create(c).await; })),
                    cmd::RESUME  => Box::new(|a, c| Box::pin(async move { a.on_resume(c).await; })),
                    cmd::PAUSE   => Box::new(|a, c| Box::pin(async move { a.on_pause(c).await; })),
                    cmd::DESTROY => Box::new(|a, c| Box::pin(async move { a.on_destroy(c).await; })),
                    other => panic!("unexpected cmd_kind in test fixture: {other:#x}"),
                };
                let outcome = if cmd_kind == cmd::DESTROY {
                    DispatchOutcome::Destroy
                } else {
                    DispatchOutcome::NoBytes
                };
                let fut = build_async_future(s, outcome, body);
                s.pending.insert(dispatch_id, PendingDispatch {
                    future: fut,
                    completed: None,
                    cmd_kind,
                });
            });
            with_dispatches(|d| { d.insert(dispatch_id, handle); });
            let _ = drive_pending(dispatch_id);
            let mut len: u32 = 0;
            let _ = get_result(dispatch_id, &mut len as *mut u32);
        }
    }

    /// Helper: an empty `Slot` placeholder. The mem::replace dance in
    /// `run_lifecycle_with` needs *something* shaped like a Slot to
    /// swap the real one out of, and a Slot can't be `Default` (the
    /// activity field is `Box<dyn Activity>`).
    fn empty_slot() -> Slot {
        Slot {
            pending: HashMap::new(),
            spawned: Vec::new(),
            activity: Box::new(NopActivity::default()),
            ctx: Context {
                metrics: Metrics::default(),
                timestamp_ms: 0.0,
                pointer: None,
                class: "x",
                handle: 0,
            },
            paint_buf: Vec::new(),
            save_buf: Vec::new(),
            last_outcome: DispatchOutcome::NoBytes,
        }
    }

    #[test]
    fn resumed_scope_spawn_is_polled_by_drive_spawned() {
        // Spawn from on_resume with Resumed scope. drive_pending only
        // *queues* the spawned task into the slot (via
        // drain_spawn_queue); the host's akiva_app_drive_spawned is
        // what actually polls. Verify that path.
        let counter = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let activity = Box::new(SpawnInActivity {
            spawn_in_create: None,
            spawn_in_resume: Some((crate::activity::ScopeKind::Resumed, counter.clone())),
        });
        run_lifecycle_with(0xa01, activity, &[(1, cmd::CREATE), (2, cmd::RESUME)]);
        // Pre-drive: not polled yet (drive_pending just queued).
        assert_eq!(counter.get(), 0);
        // Each drive_spawned call advances the counter by 1.
        let _ = drive_spawned();
        assert_eq!(counter.get(), 1);
        let _ = drive_spawned();
        assert_eq!(counter.get(), 2);
        // Cleanup.
        run_lifecycle_with(0xa01, Box::new(SpawnInActivity::default()),
            &[(99, cmd::DESTROY)]);
    }

    #[test]
    fn pause_drops_resumed_scope_spawns() {
        // After PAUSE resolves, drive_spawned should NOT advance the
        // counter — the Resumed-scoped task got dropped.
        let counter = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let activity = Box::new(SpawnInActivity {
            spawn_in_create: None,
            spawn_in_resume: Some((crate::activity::ScopeKind::Resumed, counter.clone())),
        });
        run_lifecycle_with(0xa02, activity,
            &[(1, cmd::CREATE), (2, cmd::RESUME), (3, cmd::PAUSE)]);
        let frozen = counter.get();
        let _ = drive_spawned();
        let _ = drive_spawned();
        assert_eq!(counter.get(), frozen,
            "Resumed-scoped task must be dropped on PAUSE; further drive_spawned must be a no-op for it");
        // Cleanup.
        run_lifecycle_with(0xa02, Box::new(SpawnInActivity::default()),
            &[(99, cmd::DESTROY)]);
    }

    #[test]
    fn pause_keeps_activity_scope_spawns_alive() {
        // Activity-scoped tasks survive PAUSE and keep being polled.
        let counter = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let activity = Box::new(SpawnInActivity {
            spawn_in_create: Some((crate::activity::ScopeKind::Activity, counter.clone())),
            spawn_in_resume: None,
        });
        run_lifecycle_with(0xa03, activity,
            &[(1, cmd::CREATE), (2, cmd::RESUME), (3, cmd::PAUSE)]);
        let before = counter.get();
        let _ = drive_spawned();
        assert_eq!(counter.get(), before + 1,
            "Activity-scoped task must keep ticking across PAUSE");
        // Cleanup.
        run_lifecycle_with(0xa03, Box::new(SpawnInActivity::default()),
            &[(99, cmd::DESTROY)]);
    }

    #[test]
    fn destroy_drops_every_spawn() {
        // Activity-scoped task spawned in on_create. After DESTROY the
        // slot is gone and drive_spawned shouldn't poll the future.
        let counter = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let activity = Box::new(SpawnInActivity {
            spawn_in_create: Some((crate::activity::ScopeKind::Activity, counter.clone())),
            spawn_in_resume: None,
        });
        run_lifecycle_with(0xa04, activity,
            &[(1, cmd::CREATE), (2, cmd::DESTROY)]);
        let frozen = counter.get();
        let _ = drive_spawned();
        let _ = drive_spawned();
        assert_eq!(counter.get(), frozen,
            "DESTROY evicts the slot, including all spawned tasks regardless of scope");
        // Slot should be gone.
        assert!(with_slots(|s| !s.contains_key(&0xa04u32)));
    }

    #[test]
    fn drive_spawned_returns_pending_count() {
        // Sanity: drive_spawned reports the number of tasks still in
        // flight, not the number it polled.
        let counter = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let activity = Box::new(SpawnInActivity {
            spawn_in_create: Some((crate::activity::ScopeKind::Activity, counter.clone())),
            spawn_in_resume: None,
        });
        run_lifecycle_with(0xa05, activity,
            &[(1, cmd::CREATE)]);
        // The Pending future never resolves, so drive_spawned reports 1.
        let n = drive_spawned();
        assert_eq!(n, 1);
        // Cleanup.
        run_lifecycle_with(0xa05, Box::new(SpawnInActivity::default()),
            &[(99, cmd::DESTROY)]);
        assert_eq!(drive_spawned(), 0,
            "after destroy the slot is gone — no pending tasks anywhere");
    }

    /// Stone 11.6.5 (#12) — `slot.pending` is a HashMap keyed by
    /// dispatch_id, so two callbacks with the same handle could
    /// coexist if the framework ever permitted it. The framework's
    /// PendingTransition discipline still serialises in production,
    /// but the surface no longer hard-codes "one in-flight dispatch"
    /// as a structural invariant. Verify the data structure.
    #[test]
    fn slot_pending_holds_concurrent_entries_keyed_by_dispatch_id() {
        // Build a slot manually, install two pending entries with
        // distinct dispatch_ids, verify both survive the lookup and
        // that dropping one leaves the other intact.
        let handle = 0xc12u32;
        let class_static = intern_class("ConcurrentDispatch");
        let mut slot = Box::new(Slot {
            pending: HashMap::new(),
            spawned: Vec::new(),
            activity: Box::new(NopActivity::default()),
            ctx: Context {
                metrics: Metrics::default(),
                timestamp_ms: 0.0,
                pointer: None,
                class: class_static,
                handle,
            },
            paint_buf: Vec::new(),
            save_buf: Vec::new(),
            last_outcome: DispatchOutcome::NoBytes,
        });
        slot.pending.insert(101, PendingDispatch {
            future: Box::pin(async move { DispatchOutcome::NoBytes }),
            completed: None,
            cmd_kind: cmd::RESUME,
        });
        slot.pending.insert(202, PendingDispatch {
            future: Box::pin(async move { DispatchOutcome::Paint }),
            completed: Some(DispatchOutcome::Paint),
            cmd_kind: cmd::RENDER,
        });
        with_slots(|s| { s.insert(handle, slot); });
        with_dispatches(|d| {
            d.insert(101, handle);
            d.insert(202, handle);
        });

        // Both polls succeed independently, returning the right state
        // for each entry without aliasing.
        let p101 = poll_dispatch(101);
        let s101 = (p101 & 0xFFFF_FFFF) as u32;
        // 101 was Pending — a no-op future returning NoBytes resolves
        // on first poll, so we expect READY here.
        assert_eq!(s101, op_kinds::status::READY);

        let p202 = poll_dispatch(202);
        let s202 = (p202 & 0xFFFF_FFFF) as u32;
        assert_eq!(s202, op_kinds::status::READY);

        // Dropping 101 via get_result must NOT touch 202.
        let mut len: u32 = 0;
        get_result(101, &mut len as *mut u32);
        with_slots(|s| {
            let slot = s.get(&handle).unwrap();
            assert!(!slot.pending.contains_key(&101));
            assert!(slot.pending.contains_key(&202),
                "the OTHER dispatch in the same slot must survive a peer's drop");
        });

        // Cleanup.
        get_result(202, &mut len as *mut u32);
        with_slots(|s| { s.remove(&handle); });
        with_dispatches(|d| {
            d.remove(&101);
            d.remove(&202);
        });
    }

    #[test]
    fn intern_class_dedupes() {
        let a = intern_class("X");
        let b = intern_class("X");
        assert!(core::ptr::eq(a, b));
        let c = intern_class("Y");
        assert!(!core::ptr::eq(a, c));
    }
}
