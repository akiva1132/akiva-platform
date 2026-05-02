//! The `Activity` trait app authors implement.
//!
//! An activity is one screen worth of UI + interaction. Over the course
//! of its life the runtime drives it through lifecycle callbacks
//! (`on_create` → `on_resume` → `on_pause` → ...) interleaved with input
//! events and render requests. All callbacks have default no-op impls so
//! a minimal app overrides only what it cares about — typically just
//! `on_render`.
//!
//! Stone 11.2 — every callback is `async fn`. The SDK runtime drives
//! each one with the [`block_on_ready`] poller until host imports
//! become genuinely async (Stone 11.3); until then every future
//! resolves on its first poll. The user-facing trait stays the same
//! when 11.3 lands a real executor.
//!
//! `async_trait` is used to box the returned futures so the runtime
//! can keep the per-handle slot table polymorphic over the concrete
//! activity type (`Box<dyn Activity>`). A follow-up will likely make
//! the slot table generic over the macro's `T` and drop the boxing —
//! that's a refactor independent of the trait surface.
//!
//! See [docs/activity-model.md] for the state-machine rules and
//! [docs/abi.md §2.2] for how each callback maps to a raw wasm export.

use core::future::Future;
use std::collections::HashMap;

use async_trait::async_trait;

use crate::canvas::Canvas;

// ── Public data types ─────────────────────────────────────────────────

/// Surface the activity is painting onto. Filled in by the runtime before
/// every callback; activities read it from `Context::metrics`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics {
    pub width_px: u32,
    pub height_px: u32,
    /// Device pixel ratio. `1.0` on a 160dpi reference surface.
    pub density: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase { Down, Move, Up, Cancel }

#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub phase: PointerPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase { Down, Repeat, Up }

#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub code: u32,
    pub modifiers: u8,
    pub phase: KeyPhase,
}

/// Per-callback context the runtime constructs fresh each tick. Carries
/// display metrics, a frame timestamp, and (when relevant) the most
/// recent pointer state — so callbacks that need to look at the pointer
/// outside `on_pointer` (e.g. during render) don't need to thread state.
///
/// `class` is the activity class name the host registered this handle
/// under (the string from the manifest's `[[activities]].class`). Useful
/// when one Rust type backs multiple declared activity classes.
pub struct Context<'a> {
    pub metrics: Metrics,
    pub timestamp_ms: f64,
    pub pointer: Option<PointerEvent>,
    pub class: &'a str,
    /// Handle the runtime passed to the current callback. Kept
    /// crate-private because it's only useful for host imports that need
    /// to name the activity (see [`Context::finish`]); apps shouldn't
    /// have to thread it around or compare two handles themselves.
    pub(crate) handle: u32,
}

// ── block_on_ready driver ─────────────────────────────────────────────

/// Drive `f` to completion with a no-op waker. Panics on `Pending`.
///
/// Pre-P0 #4 the entire SDK runtime drove activity callbacks through
/// this poller — and the panic was a canary that triggered the moment
/// any host import returned `Pending`. P0 #4 moved the actual dispatch
/// path to a real async executor (`crate::runtime`), so the panic is
/// no longer load-bearing for that path; this fn now wraps
/// `crate::executor::run_to_ready` purely as a back-compat re-export
/// for user code that imports the symbol (e.g. tests that drive a
/// future end-to-end inline).
///
/// New code should prefer `crate::executor::run_to_ready` directly,
/// or — for futures that may genuinely suspend — let the SDK runtime
/// drive them via `akiva_app_dispatch` + `akiva_app_poll_dispatch`.
pub fn block_on_ready<F: Future>(f: F) -> F::Output {
    crate::executor::run_to_ready(f)
}

// ── Lifecycle scopes ──────────────────────────────────────────────────
//
// Two task scopes — `activity_scope` and `resumed_scope` — modelled on
// Android's `lifecycleScope` / `viewModelScope` and SwiftUI's
// `.task(id:)`. The contract from docs/isolation-design.md §4.7:
//
//   - `activity_scope`: tasks live until on_destroy.
//   - `resumed_scope`:  tasks live until on_pause.
//
// One-shot await (`storage::read("k").await` directly inside a
// callback) doesn't need a scope — it dies when the calling fn
// returns by definition.
//
// Stone 11.6.5 (#11) put cancellation teeth on these. Each spawned
// future is stored in the activity's slot until it resolves, the
// activity hits the matching cancellation edge, or the activity
// destroys. The runtime ([`crate::runtime`]) owns the queue and
// drives every spawned task once per host wake-up; the SDK exports
// `akiva_app_drive_spawned` for the worker shell to pump after each
// `host_op_done`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// Outlives every callback except `on_destroy` — the activity itself.
    Activity,
    /// Active only between `on_resume` and the matching `on_pause`.
    Resumed,
}

/// Task scope handed back by [`Context::activity_scope`] /
/// [`Context::resumed_scope`]. Carries the activity handle and the
/// cancellation policy together so [`TaskScope::spawn`] can route
/// the future to the right per-activity queue without a thread-local
/// lookup.
///
/// Cheap to copy (one `u32` + one enum tag) — apps that want to spawn
/// from inside a closure can clone the scope value into the closure's
/// captures.
#[derive(Clone, Copy)]
pub struct TaskScope {
    kind: ScopeKind,
    handle: u32,
}

impl TaskScope {
    /// Spawn `fut` under this scope's cancellation policy.
    ///
    /// The future is stored in the activity's slot and polled once per
    /// host wake-up (each `host_op_done` arrival from the kernel-worker
    /// triggers a sweep). When the activity hits the cancellation edge
    /// for this scope (`on_pause` for `Resumed` scopes, `on_destroy`
    /// for `Activity` scopes) the SDK runtime drops
    /// the future without further polling — any local state the future
    /// held drops with it, and any in-flight host op the future was
    /// awaiting is left to land and get discarded.
    ///
    /// `'static` is required because the future is retained across
    /// callbacks. Apps that need to share state with the activity
    /// instance pass it through `Rc<RefCell<...>>` or similar — the
    /// SDK doesn't try to magic-borrow `&mut self`.
    pub fn spawn<F: Future<Output = ()> + 'static>(&self, fut: F) {
        crate::runtime::spawn_in_slot(self.handle, self.kind, Box::pin(fut));
    }
}

// ── Context impl ──────────────────────────────────────────────────────

impl<'a> Context<'a> {
    /// Scope whose tasks live until [`Activity::on_destroy`]. Tasks
    /// spawned here survive `on_pause` / `on_resume` cycles — use this
    /// for state that should outlive a brief background trip (e.g. an
    /// unstarted-yet network request the user will see when they come
    /// back).
    pub fn activity_scope(&self) -> TaskScope {
        TaskScope { kind: ScopeKind::Activity, handle: self.handle }
    }

    /// Scope whose tasks live only while the activity is in the
    /// `Resumed` state. Tasks are cancelled at `on_pause` and a fresh
    /// scope is opened the next time `on_resume` fires. Use this for
    /// listeners / polling loops that only make sense when the user
    /// is actively looking at the activity.
    pub fn resumed_scope(&self) -> TaskScope {
        TaskScope { kind: ScopeKind::Resumed, handle: self.handle }
    }

    // ── Affordance API ────────────────────────────────────────────────
    //
    // In Stone 11.2 each one calls the underlying host import
    // directly — the `async fn` is degenerate (one synchronous call
    // wrapped in a future that's Ready on first poll) but the
    // `.await` is locked into app source today so Stone 11.3's async
    // wire is a body change, not an API change.
    //
    // `&mut self` per docs/isolation-design.md §4.1 — Stone 11.3 will
    // need to track the in-flight request future on `Context` to
    // resume the awaiting task when the host acks. Apps already pass
    // `ctx: &mut Context<'_>` to their callbacks, so the borrow shape
    // matches.

    /// Ask the platform to tear this activity down. Fires after the
    /// current callback returns — the framework runs
    /// `on_pause → on_stop → on_destroy` in order (or, if called from
    /// inside `on_create`, jumps straight to `on_destroy` via the
    /// `Created → Destroyed` shortcut documented in
    /// [docs/activity-model.md §2](../../docs/activity-model.md)).
    ///
    /// No-op on non-wasm targets so unit tests that build with the
    /// SDK can call it without a host.
    pub async fn finish(&mut self) {
        crate::runtime::host_finish(self.handle);
    }

    /// Resolve `intent` through `IntentRouter` and launch the matching
    /// activity on top of this one. The current activity keeps its
    /// handle and transitions to `Paused` while the new activity comes
    /// up. If the resolver picks a reuse path (`SingleTop` on this
    /// same class, or `SingleTask` / `SingleInstance` for an existing
    /// instance) the target sees `on_new_intent` rather than a fresh
    /// bring-up. See [docs/intents.md §8](../../docs/intents.md).
    pub async fn start_activity(&mut self, intent: &crate::intent::Intent) {
        crate::runtime::host_start_activity(self.handle, intent);
    }

    /// Launch `intent` and keep the door open for a result. When the
    /// callee finishes (either by calling [`Context::set_result`] +
    /// [`Context::finish`] or by being closed without a result),
    /// `on_activity_result(ctx, request_code, result)` fires on this
    /// activity. Pick a `request_code` per call site — the callee
    /// never sees it; it's only for the caller to disambiguate which
    /// launch the arriving result belongs to. See
    /// [docs/activity-model.md](../../docs/activity-model.md).
    pub async fn start_activity_for_result(
        &mut self, intent: &crate::intent::Intent, request_code: u32,
    ) {
        crate::runtime::host_start_activity_for_result(self.handle, intent, request_code);
    }

    /// Stash the result this activity wants to hand back. Only
    /// meaningful if this activity was launched via
    /// [`Context::start_activity_for_result`]; otherwise the framework
    /// silently discards the bytes. Can be called more than once in a
    /// single lifetime — only the final call's payload is delivered.
    pub async fn set_result(&mut self, result: &crate::result::ActivityResult) {
        crate::runtime::host_set_result(self.handle, result);
    }
}

// ── The trait ─────────────────────────────────────────────────────────

/// What every Akiva app implements. All callbacks are `async fn` —
/// the SDK runtime polls them via [`block_on_ready`] in Stone 11.2,
/// and via the embassy executor + worker-per-app dispatch in Stone
/// 11.3+.
#[async_trait(?Send)]
pub trait Activity: 'static {
    // ── Lifecycle (docs/activity-model.md §2) ──────────────────────────
    async fn on_create(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    async fn on_start(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    async fn on_resume(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    async fn on_pause(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    async fn on_stop(&mut self, ctx: &mut Context<'_>)    { let _ = ctx; }
    async fn on_destroy(&mut self, ctx: &mut Context<'_>) { let _ = ctx; }

    // ── Input ───────────────────────────────────────────────────────────
    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        let _ = (ctx, ev);
    }
    async fn on_key(&mut self, ctx: &mut Context<'_>, ev: KeyEvent) {
        let _ = (ctx, ev);
    }
    async fn on_resize(&mut self, ctx: &mut Context<'_>, metrics: Metrics) {
        let _ = (ctx, metrics);
    }

    // ── Render ──────────────────────────────────────────────────────────
    /// Paint one frame. Called at the runtime's frame rate (rAF, typically
    /// 60 Hz). Must be fast — all ops are serialised immediately.
    async fn on_render(&mut self, ctx: &mut Context<'_>, canvas: &mut Canvas<'_>) {
        let _ = (ctx, canvas);
    }

    // ── Intent redelivery (Stone 9.4) ───────────────────────────────────
    /// Called when the activity is re-launched while still alive — for
    /// a `SingleTop` top re-click, or (Stone 9.2) a matching
    /// `SingleTask`/`SingleInstance` reuse. The activity does not
    /// transition through `on_create`/`on_resume` again; it receives
    /// the fresh intent in place. Default is a no-op for apps that
    /// don't distinguish reuse from first-launch.
    async fn on_new_intent(&mut self, ctx: &mut Context<'_>, intent: &crate::intent::Intent) {
        let _ = (ctx, intent);
    }

    // ── Activity result (Stone 9.5) ─────────────────────────────────────
    /// Called on the *caller* after an activity this one launched via
    /// [`Context::start_activity_for_result`] finishes. `request_code`
    /// is whatever was passed at launch; `result` is either the payload
    /// the callee [`Context::set_result`]-ed or a `RESULT_CANCELED`
    /// synthesis if it finished without producing one. Default no-op
    /// for apps that don't launch for results.
    async fn on_activity_result(
        &mut self,
        ctx: &mut Context<'_>,
        request_code: u32,
        result: &crate::result::ActivityResult,
    ) {
        let _ = (ctx, request_code, result);
    }

    // ── Saved state — synchronous on purpose ──────────────────────────
    //
    // The macro reads/writes the saved-state buffer around the
    // dispatch path (see crate::runtime::on_save_state); making these
    // async would force a future-allocation per save without any
    // benefit. Keep them sync.
    fn on_save_state(&self, out: &mut SavedState) { let _ = out; }
    fn on_restore_state(&mut self, saved: &SavedState) { let _ = saved; }
}

// ── Saved state ────────────────────────────────────────────────────────
//
// Small typed key/value bag an activity hands back across a process kill.
// Wire format (all little-endian):
//
//   u32  count
//   for each entry:
//       u32  key_len
//       <key_len> UTF-8 bytes
//       u8   tag      (see `tag` module below)
//       <payload per tag>
//
// Kept intentionally small — the framework treats the whole thing as an
// opaque blob and never inspects individual entries. Decoding happens on
// the other side of the ABI, inside the app's own wasm.

mod tag {
    pub const NULL:   u8 = 0;
    pub const BOOL:   u8 = 1;
    pub const INT:    u8 = 2;
    pub const FLOAT:  u8 = 3;
    pub const STRING: u8 = 4;
    pub const BYTES:  u8 = 5;
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SavedState {
    pub entries: HashMap<String, SavedValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SavedValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

/// Errors that can come out of [`SavedState::decode`]. Exposed as an enum
/// so callers distinguish "stream truncated" (probably a host bug) from
/// "tag we don't recognise" (probably a version mismatch).
#[derive(Debug, PartialEq, Eq)]
pub enum SavedStateDecodeError {
    Truncated,
    BadUtf8,
    UnknownTag(u8),
    /// Encoded blob would exceed [`SavedState::MAX_BYTES`]; refused to
    /// avoid unbounded app memory growth on untrusted input.
    TooLarge,
}

impl SavedState {
    /// Budget the runtime honours. 64 KiB matches Android's pragmatic
    /// upper bound on `onSaveInstanceState` — large enough for UI state
    /// (scroll offsets, text buffers) and small enough to keep writes
    /// cheap. Apps that need more should use persistent storage instead.
    pub const MAX_BYTES: usize = 64 * 1024;

    // ── Typed putters ────────────────────────────────────────────────

    pub fn put_bool(&mut self, key: &str, v: bool) {
        self.entries.insert(key.into(), SavedValue::Bool(v));
    }
    pub fn put_int(&mut self, key: &str, v: i64) {
        self.entries.insert(key.into(), SavedValue::Int(v));
    }
    pub fn put_float(&mut self, key: &str, v: f64) {
        self.entries.insert(key.into(), SavedValue::Float(v));
    }
    pub fn put_string(&mut self, key: &str, v: impl Into<String>) {
        self.entries.insert(key.into(), SavedValue::String(v.into()));
    }
    pub fn put_bytes(&mut self, key: &str, v: impl Into<Vec<u8>>) {
        self.entries.insert(key.into(), SavedValue::Bytes(v.into()));
    }

    // ── Typed getters ────────────────────────────────────────────────

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.entries.get(key)? { SavedValue::Bool(v) => Some(*v), _ => None }
    }
    pub fn get_int(&self, key: &str) -> Option<i64> {
        match self.entries.get(key)? { SavedValue::Int(v) => Some(*v), _ => None }
    }
    pub fn get_float(&self, key: &str) -> Option<f64> {
        match self.entries.get(key)? { SavedValue::Float(v) => Some(*v), _ => None }
    }
    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.entries.get(key)? { SavedValue::String(v) => Some(v), _ => None }
    }
    pub fn get_bytes(&self, key: &str) -> Option<&[u8]> {
        match self.entries.get(key)? { SavedValue::Bytes(v) => Some(v), _ => None }
    }

    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    // ── Wire codec ───────────────────────────────────────────────────

    /// Serialise to the wire format documented above. Key iteration order
    /// is sorted so the output is deterministic — useful for tests, and
    /// means an app that writes the same logical state twice produces
    /// byte-identical blobs (friendlier to storage dedup).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.entries.len() * 16);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        for key in keys {
            let value = &self.entries[key];
            let bytes = key.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
            match value {
                SavedValue::Null => out.push(tag::NULL),
                SavedValue::Bool(b) => {
                    out.push(tag::BOOL);
                    out.push(if *b { 1 } else { 0 });
                }
                SavedValue::Int(i) => {
                    out.push(tag::INT);
                    out.extend_from_slice(&i.to_le_bytes());
                }
                SavedValue::Float(f) => {
                    out.push(tag::FLOAT);
                    out.extend_from_slice(&f.to_le_bytes());
                }
                SavedValue::String(s) => {
                    out.push(tag::STRING);
                    let sb = s.as_bytes();
                    out.extend_from_slice(&(sb.len() as u32).to_le_bytes());
                    out.extend_from_slice(sb);
                }
                SavedValue::Bytes(b) => {
                    out.push(tag::BYTES);
                    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    out.extend_from_slice(b);
                }
            }
        }
        out
    }

    /// Decode from the wire format. The inverse of [`Self::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self, SavedStateDecodeError> {
        if bytes.len() > Self::MAX_BYTES {
            return Err(SavedStateDecodeError::TooLarge);
        }
        let mut r = Reader { buf: bytes };
        let count = r.read_u32()?;
        let mut entries = HashMap::with_capacity(count as usize);
        for _ in 0..count {
            let key_len = r.read_u32()? as usize;
            let key_bytes = r.read_bytes(key_len)?;
            let key = core::str::from_utf8(key_bytes)
                .map_err(|_| SavedStateDecodeError::BadUtf8)?
                .to_string();
            let t = r.read_u8()?;
            let value = match t {
                tag::NULL => SavedValue::Null,
                tag::BOOL => SavedValue::Bool(r.read_u8()? != 0),
                tag::INT => SavedValue::Int(i64::from_le_bytes(
                    r.read_array::<8>()?,
                )),
                tag::FLOAT => SavedValue::Float(f64::from_le_bytes(
                    r.read_array::<8>()?,
                )),
                tag::STRING => {
                    let n = r.read_u32()? as usize;
                    let sb = r.read_bytes(n)?;
                    SavedValue::String(
                        core::str::from_utf8(sb)
                            .map_err(|_| SavedStateDecodeError::BadUtf8)?
                            .to_string(),
                    )
                }
                tag::BYTES => {
                    let n = r.read_u32()? as usize;
                    SavedValue::Bytes(r.read_bytes(n)?.to_vec())
                }
                other => return Err(SavedStateDecodeError::UnknownTag(other)),
            };
            entries.insert(key, value);
        }
        Ok(SavedState { entries })
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn read_u8(&mut self) -> Result<u8, SavedStateDecodeError> {
        let b = *self.buf.first().ok_or(SavedStateDecodeError::Truncated)?;
        self.buf = &self.buf[1..];
        Ok(b)
    }
    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], SavedStateDecodeError> {
        if self.buf.len() < N { return Err(SavedStateDecodeError::Truncated); }
        let (head, tail) = self.buf.split_at(N);
        self.buf = tail;
        Ok(head.try_into().expect("len checked"))
    }
    fn read_u32(&mut self) -> Result<u32, SavedStateDecodeError> {
        Ok(u32::from_le_bytes(self.read_array::<4>()?))
    }
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], SavedStateDecodeError> {
        if self.buf.len() < n { return Err(SavedStateDecodeError::Truncated); }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }
}

// ── Backward-compat alias ─────────────────────────────────────────────
// The original scaffold called the context `ActivityContext`. Keeping the
// alias means existing imports of the old name still resolve.
pub type ActivityContext<'a> = Context<'a>;

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use core::pin::Pin;
    use core::task::{Context as TaskContext, Poll};

    // ── block_on_ready / TaskScope / async ctx methods ────────────────

    #[test]
    fn block_on_ready_returns_immediate() {
        let v = block_on_ready(async { 42 });
        assert_eq!(v, 42);
    }

    #[test]
    fn block_on_ready_threads_value_through_await() {
        // A nested await chain still resolves in one poll because every
        // step is itself Ready-on-first-poll.
        async fn inner() -> i32 { 7 }
        async fn outer() -> i32 { inner().await + 35 }
        let v = block_on_ready(outer());
        assert_eq!(v, 42);
    }

    #[test]
    #[should_panic(expected = "future returned Pending")]
    fn block_on_ready_panics_on_pending() {
        struct Pending;
        impl Future for Pending {
            type Output = ();
            fn poll(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<()> {
                Poll::Pending
            }
        }
        block_on_ready(Pending);
    }

    #[test]
    fn context_exposes_distinct_scope_kinds() {
        // `activity_scope` and `resumed_scope` carry distinct kinds so
        // the runtime can apply the right cancellation policy. Both
        // scopes capture the calling context's handle, which is what
        // routes a spawned task to the correct slot.
        let ctx = Context {
            metrics: Metrics::default(),
            timestamp_ms: 0.0,
            pointer: None,
            class: "",
            handle: 7,
        };
        let a = ctx.activity_scope();
        let r = ctx.resumed_scope();
        assert_eq!(a.kind, ScopeKind::Activity);
        assert_eq!(r.kind, ScopeKind::Resumed);
        assert_eq!(a.handle, 7);
        assert_eq!(r.handle, 7);
    }

    #[test]
    fn async_ctx_methods_complete_in_one_poll() {
        // Stone 11.2 invariant: every `async fn` on Context resolves
        // synchronously because the underlying host imports are no-ops
        // on non-wasm and one-shot sync calls on wasm. block_on_ready
        // would panic on Pending, so this round-trip proves the
        // current API can't accidentally introduce real awaits.
        let mut ctx = Context {
            metrics: Metrics::default(),
            timestamp_ms: 0.0,
            pointer: None,
            class: "",
            handle: 1,
        };
        // The four affordance methods, awaited in sequence. Hosts on
        // non-wasm fall back to no-ops so this exercises the surface
        // shape without needing a kernel.
        block_on_ready(async {
            ctx.finish().await;
            ctx.start_activity(&crate::intent::Intent::new("akiva.intent.action.MAIN")).await;
            ctx.start_activity_for_result(
                &crate::intent::Intent::new("akiva.intent.action.MAIN"),
                42,
            ).await;
            ctx.set_result(&crate::result::ActivityResult::canceled()).await;
        });
    }

    // ── SavedState ────────────────────────────────────────────────────

    #[test]
    fn empty_state_roundtrips() {
        let state = SavedState::default();
        let bytes = state.encode();
        assert_eq!(bytes, vec![0, 0, 0, 0]); // count=0
        let decoded = SavedState::decode(&bytes).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn all_variants_roundtrip() {
        let mut state = SavedState::default();
        state.entries.insert("null".into(), SavedValue::Null);
        state.put_bool("flag", true);
        state.put_int("counter", -42);
        state.put_float("ratio", 1.5);
        state.put_string("title", "שלום world");
        state.put_bytes("blob", vec![0x00, 0xff, 0x42]);

        let bytes = state.encode();
        let decoded = SavedState::decode(&bytes).unwrap();
        assert_eq!(decoded.entries.len(), 6);
        assert_eq!(decoded.entries.get("null"), Some(&SavedValue::Null));
        assert_eq!(decoded.get_bool("flag"), Some(true));
        assert_eq!(decoded.get_int("counter"), Some(-42));
        assert_eq!(decoded.get_float("ratio"), Some(1.5));
        assert_eq!(decoded.get_string("title"), Some("שלום world"));
        assert_eq!(decoded.get_bytes("blob"), Some(&[0x00u8, 0xff, 0x42][..]));
    }

    #[test]
    fn encoding_is_deterministic_across_insertion_order() {
        let mut a = SavedState::default();
        a.put_int("x", 1);
        a.put_int("y", 2);
        let mut b = SavedState::default();
        b.put_int("y", 2);
        b.put_int("x", 1);
        assert_eq!(a.encode(), b.encode());
    }

    #[test]
    fn decode_rejects_truncated_header() {
        assert_eq!(
            SavedState::decode(&[0, 0]),
            Err(SavedStateDecodeError::Truncated),
        );
    }

    #[test]
    fn decode_rejects_truncated_string_payload() {
        // count=1, key_len=3 "abc", tag=STRING, str_len=10, only 2 bytes
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        bytes.push(tag::STRING);
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.extend_from_slice(&[0xaa, 0xbb]);
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::Truncated),
        );
    }

    #[test]
    fn decode_rejects_unknown_tag() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(b'k');
        bytes.push(0x7f);
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::UnknownTag(0x7f)),
        );
    }

    #[test]
    fn decode_rejects_bad_utf8_key() {
        let mut bytes = vec![];
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.push(tag::NULL);
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::BadUtf8),
        );
    }

    #[test]
    fn decode_refuses_blob_larger_than_max() {
        let bytes = vec![0u8; SavedState::MAX_BYTES + 1];
        assert_eq!(
            SavedState::decode(&bytes),
            Err(SavedStateDecodeError::TooLarge),
        );
    }
}
