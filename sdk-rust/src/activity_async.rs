//! `AsyncActivity` — Stone 11.2 async-native lifecycle trait.
//!
//! Parallel path to [`crate::activity::Activity`] during the migration:
//! new apps use `AsyncActivity` + [`akiva_async_app!`]; legacy apps stay
//! on the sync trait until they're ported. Both produce the same wasm
//! exports (the macros generate identical `extern "C"` symbols), so the
//! framework side is unchanged.
//!
//! In Stone 11.2 every host import is still synchronous underneath
//! ([`crate::runtime::host_start_activity`] etc.), which means every
//! `async fn` in this trait completes in a single `Future::poll`. The
//! [`block_on_ready`] driver therefore never needs to actually park —
//! it polls once with a no-op waker and asserts `Poll::Ready`. When
//! Stone 11.3 makes host imports return `Pending`, this driver gets
//! replaced with a real executor; the user-facing trait stays the
//! same.
//!
//! `async_trait` is used to box the returned futures so the runtime can
//! keep the per-handle slot table polymorphic over the concrete activity
//! type (`Box<dyn AsyncActivity>`). A follow-up in 11.2 will likely make
//! the slot table generic over the macro's `T` and drop the boxing —
//! but that's a refactor independent of the trait surface, so we eat
//! one heap alloc per lifecycle call to unblock the spike.

use core::future::Future;
use core::task::{Context as TaskContext, Poll, RawWaker, RawWakerVTable, Waker};

use async_trait::async_trait;

use crate::activity::{Context, KeyEvent, Metrics, PointerEvent, SavedState};
use crate::canvas::Canvas;

/// Async-native counterpart to [`Activity`](crate::activity::Activity).
///
/// All callbacks are `async fn`; the SDK runtime drives each one with
/// the [`block_on_ready`] poller until host imports become genuinely
/// async (Stone 11.3). Default impls are no-ops so apps override only
/// what they care about — same ergonomics as the sync trait.
#[async_trait(?Send)]
pub trait AsyncActivity: 'static {
    async fn on_create(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    async fn on_start(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    async fn on_resume(&mut self, ctx: &mut Context<'_>)  { let _ = ctx; }
    async fn on_pause(&mut self, ctx: &mut Context<'_>)   { let _ = ctx; }
    async fn on_stop(&mut self, ctx: &mut Context<'_>)    { let _ = ctx; }
    async fn on_destroy(&mut self, ctx: &mut Context<'_>) { let _ = ctx; }

    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        let _ = (ctx, ev);
    }
    async fn on_key(&mut self, ctx: &mut Context<'_>, ev: KeyEvent) {
        let _ = (ctx, ev);
    }
    async fn on_resize(&mut self, ctx: &mut Context<'_>, metrics: Metrics) {
        let _ = (ctx, metrics);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, canvas: &mut Canvas<'_>) {
        let _ = (ctx, canvas);
    }

    async fn on_new_intent(&mut self, ctx: &mut Context<'_>, intent: &crate::intent::Intent) {
        let _ = (ctx, intent);
    }
    async fn on_activity_result(
        &mut self,
        ctx: &mut Context<'_>,
        request_code: u32,
        result: &crate::result::ActivityResult,
    ) {
        let _ = (ctx, request_code, result);
    }

    fn on_save_state(&self, out: &mut SavedState) { let _ = out; }
    fn on_restore_state(&mut self, saved: &SavedState) { let _ = saved; }
}

/// Drive `f` to completion with a no-op waker.
///
/// Sound for Stone 11.2 only: the contract is that every future returned
/// by an `AsyncActivity` callback resolves on its first poll, because
/// every host import it can `await` is still synchronous underneath. If
/// that invariant breaks (Stone 11.3 onward, when host calls return
/// real `Pending`), this driver gets replaced — the panic is the
/// canary that flags the violation early.
pub fn block_on_ready<F: Future>(f: F) -> F::Output {
    static NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
        // clone
        |_| RawWaker::new(core::ptr::null(), &NOOP_VTABLE),
        // wake
        |_| {},
        // wake_by_ref
        |_| {},
        // drop
        |_| {},
    );
    let raw = RawWaker::new(core::ptr::null(), &NOOP_VTABLE);
    // SAFETY: the vtable's clone/drop never deref the data pointer; null
    // is valid input for a waker that does nothing.
    let waker = unsafe { Waker::from_raw(raw) };
    let mut cx = TaskContext::from_waker(&waker);

    // Box::pin to dodge the structural-pinning gymnastics on the caller
    // side. One alloc per poll; cost rounds to noise in 11.2.
    let mut fut = Box::pin(f);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!(
            "block_on_ready: AsyncActivity future returned Pending — \
             Stone 11.2 invariant violated. Until 11.3 lands an async wire \
             every host import resolves synchronously, so callbacks must \
             complete in a single poll."
        ),
    }
}

// ── Lifecycle scopes ──────────────────────────────────────────────────
//
// Three task scopes — `activity_scope`, `resumed_scope`, one-shot —
// modelled on Android's `lifecycleScope` / `viewModelScope` and
// SwiftUI's `.task(id:)`. The contract from
// docs/isolation-design.md §4.7:
//
//   - `activity_scope`: tasks live until on_destroy.
//   - `resumed_scope`:  tasks live until on_pause.
//   - one-shot:         tasks die when the calling fn returns
//                       (already the default if you `.await` directly).
//
// In Stone 11.2 every spawned future completes in a single poll
// (host imports still sync), so cancellation is unobservable — there's
// nothing pending to drop when on_pause / on_destroy fires. The API
// surface is locked in here so apps can write the right code today;
// Stone 11.3 puts teeth on it once a real executor holds futures
// across .await points.
//
// Until then `TaskScope::spawn` is just a renamed `block_on_ready` —
// runs the future to completion right then. That matches what an
// executor would do anyway when the future is Ready-on-first-poll, so
// app behaviour will stay identical when 11.3 swaps the body.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    /// Outlives every callback except `on_destroy` — the activity itself.
    Activity,
    /// Active only between `on_resume` and the matching `on_pause`.
    Resumed,
}

/// Task scope used by [`Context::activity_scope`] /
/// [`Context::resumed_scope`]. Carries the cancellation policy as
/// data; in Stone 11.2 it's API surface only (see module comment).
pub struct TaskScope {
    kind: ScopeKind,
}

impl TaskScope {
    /// Spawn `fut` under this scope's cancellation policy.
    ///
    /// In Stone 11.2 this drives `fut` to completion synchronously via
    /// [`block_on_ready`] (`Pending` panics — every host call still
    /// resolves on first poll). Stone 11.3 replaces the body with the
    /// real executor; the signature stays the same so apps don't churn.
    ///
    /// The future is bound by the caller's lifetime (whatever borrows
    /// it captures). 11.3 will tighten this to `+ 'static` to allow
    /// cross-callback storage; until then a non-static future is fine
    /// because we poll-to-completion before this fn returns.
    pub fn spawn<F: Future<Output = ()>>(&self, fut: F) {
        let _ = self.kind;
        block_on_ready(fut);
    }
}

/// Singletons addressable as `&'static TaskScope`. Cheap, no per-handle
/// state — fits the 11.2 model where scopes are markers. When 11.3
/// makes scopes hold a queue of pending tasks they'll move into the
/// per-handle [`crate::runtime_async::Slot`] and the accessors on
/// [`Context`] will return non-static refs borrowing from there.
pub(crate) static ACTIVITY_SCOPE: TaskScope = TaskScope { kind: ScopeKind::Activity };
pub(crate) static RESUMED_SCOPE:  TaskScope = TaskScope { kind: ScopeKind::Resumed  };

impl<'a> Context<'a> {
    /// Scope whose tasks live until [`AsyncActivity::on_destroy`].
    /// Tasks spawned here survive `on_pause` / `on_resume` cycles —
    /// use this for state that should outlive a brief background trip
    /// (e.g. an unstarted-yet network request the user will see when
    /// they come back).
    pub fn activity_scope(&self) -> &'static TaskScope {
        &ACTIVITY_SCOPE
    }

    /// Scope whose tasks live only while the activity is in the
    /// `Resumed` state. Tasks are cancelled at `on_pause` and a fresh
    /// scope is opened the next time `on_resume` fires. Use this for
    /// listeners / polling loops that only make sense when the user
    /// is actively looking at the activity.
    pub fn resumed_scope(&self) -> &'static TaskScope {
        &RESUMED_SCOPE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::pin::Pin;

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
    #[should_panic(expected = "Stone 11.2 invariant violated")]
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
    fn task_scope_spawn_runs_body_before_returning() {
        // 11.2 contract: spawn drives the future to completion right
        // there. After this returns the side effect must already be
        // visible. This anchors the migration story: app code that
        // works against the 11.2 stub will keep working when 11.3
        // swaps the executor in (Ready-on-first-poll futures complete
        // identically either way).
        let scope = TaskScope { kind: ScopeKind::Activity };
        let mut fired = false;
        scope.spawn(async {
            fired = true;
        });
        assert!(fired);
    }

    #[test]
    fn context_exposes_distinct_scope_kinds() {
        // `activity_scope` and `resumed_scope` resolve to different
        // singletons — they're the same shape today but different
        // identities, so when 11.3 attaches behaviour the wiring is
        // already in place.
        assert_eq!(ACTIVITY_SCOPE.kind, ScopeKind::Activity);
        assert_eq!(RESUMED_SCOPE.kind,  ScopeKind::Resumed);
        assert_ne!(ACTIVITY_SCOPE.kind, RESUMED_SCOPE.kind);
    }
}
