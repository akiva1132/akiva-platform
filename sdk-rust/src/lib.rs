//! Akiva SDK for Rust apps.
//!
//! This crate is what app authors depend on. It wraps the App ABI v0 (see
//! [docs/abi.md §2]) in an ergonomic Rust API: an async [`Activity`]
//! trait with lifecycle + input + render callbacks, a [`Canvas`] that
//! emits paint ops in the wire format, a logging facade, and the
//! [`akiva_app!`] macro that generates the required
//! `#[no_mangle] extern "C"` glue.
//!
//! Stone 11.2 — every callback is `async fn`. The runtime drives them
//! via `block_on_ready` (panics on Pending) until Stone 11.3 brings
//! the embassy executor + async wire. The user-facing trait stays the
//! same across that transition.

pub mod activity;
pub mod akml;
pub mod app;
pub mod canvas;
pub mod clipboard;
pub mod clock;
pub mod dialog;
pub mod effects;
pub mod executor;
pub mod geolocation;
pub mod intent;
pub mod log;
pub mod net;
pub mod notification;
pub mod op_kinds;
pub mod pkgs;
pub mod random;
pub mod res;
pub mod result;
pub mod runtime;
pub mod storage;
pub mod sync;
pub mod time;
pub mod ui;
pub mod vibration;

pub use activity::{
    block_on_ready, Activity, ActivityContext, Context, KeyEvent, KeyPhase, Metrics,
    PointerEvent, PointerPhase, SavedState, TaskScope,
};
pub use app::{App, AppContext};
pub use canvas::Canvas;
pub use dialog::{DialogFuture, DialogResult};
pub use effects::{DropShadow, SurfaceEffects};
pub use intent::{Intent, IntentBuilder};
pub use result::{ActivityResult, RESULT_CANCELED, RESULT_OK};

/// Everything a minimal app needs with one `use`.
pub mod prelude {
    pub use crate::activity::{
        Activity, Context, KeyEvent, KeyPhase, Metrics, PointerEvent, PointerPhase, SavedState,
        TaskScope,
    };
    pub use crate::canvas::Canvas;
    pub use crate::dialog::{DialogFuture, DialogResult};
    pub use crate::intent::{Intent, IntentBuilder};
    pub use crate::result::{ActivityResult, RESULT_CANCELED, RESULT_OK};
    pub use crate::ui::{CrossAxis, MainAxis, NodeId, Ui, UiState};
    pub use crate::{log_debug, log_error, log_info, log_trace, log_warn};
    // Re-export `async_trait` so app code can write
    // `#[async_trait(?Send)] impl Activity for ...` without adding the
    // dep to its own Cargo.toml.
    pub use async_trait::async_trait;
}

/// Install the wasm exports required by the App ABI v0. Takes the
/// concrete [`Activity`] type the app implements; the type must be
/// `Default` so the runtime can instantiate one per handle without
/// threading a constructor through the ABI.
///
/// Expand this in `lib.rs` of each app — exactly once. The macro emits
/// `#[no_mangle]` symbols, so a second call in the same crate is a link
/// error.
///
/// ```ignore
/// use akiva_sdk::prelude::*;
///
/// #[derive(Default)]
/// struct HelloApp;
///
/// #[async_trait(?Send)]
/// impl Activity for HelloApp {
///     async fn on_render(&mut self, _ctx: &mut Context<'_>, canvas: &mut Canvas<'_>) {
///         canvas.clear(0xff202030);  // 0xAARRGGBB — alpha first
///         canvas.draw_text(20.0, 40.0, 24.0, 0xffffffff, "Hello, Akiva!");
///     }
/// }
///
/// akiva_sdk::akiva_app!(HelloApp);
/// ```
#[macro_export]
macro_rules! akiva_app {
    ($ty:ty) => {
        // ── Memory ABI ─────────────────────────────────────────────────
        #[no_mangle]
        pub extern "C" fn akiva_app_alloc(size: u32) -> *mut u8 {
            $crate::runtime::alloc(size)
        }
        #[no_mangle]
        pub extern "C" fn akiva_app_dealloc(ptr: *mut u8, size: u32) {
            $crate::runtime::dealloc(ptr, size)
        }
        #[no_mangle]
        pub extern "C" fn akiva_app_abi_version() -> *const u8 {
            $crate::runtime::ABI_VERSION.as_ptr()
        }

        // ── Unified host→app dispatch (P0 #4 — async-aware) ─────────────
        //
        // Three exports replace the legacy `dispatch -> u64 future_id`
        // shape with an async-resumable protocol. The host allocates a
        // `dispatch_id` (its own monotonic counter), passes it into
        // `dispatch`, and drives it to terminal status via repeated
        // `poll_dispatch` calls — typically once after `dispatch`, then
        // again after each `host_op_done` arrives at the worker shell.
        //
        // Return values:
        //   * `dispatch(dispatch_id, cmd, ptr, len) -> u32`
        //     `0` accepts; non-zero is a hard refusal (bad payload,
        //     unknown cmd, OOM). The host doesn't poll on refusal.
        //
        //   * `poll_dispatch(dispatch_id) -> u64` packed `(status, len)`.
        //     `status`:
        //       0 PENDING — host should call again after a `host_op_done`.
        //       1 READY   — bytes (if any) available via `get_result`.
        //       2 ERROR   — future trapped or wire bug.
        //     `len` is the byte count for `READY`/`ERROR`; 0 for
        //     `PENDING` and ack-only dispatches.
        //
        //   * `get_result(dispatch_id, out_len_ptr) -> *const u8`
        //     Two-phase fetch. Pointer + length stay valid until the
        //     dispatch_id is consumed by this call (single-shot).
        //
        // See `sdk-rust/src/runtime.rs` and `docs/abi.md §2.2` for the
        // payload encoding per cmd_kind.
        #[no_mangle]
        pub extern "C" fn akiva_app_dispatch(
            dispatch_id: u64, cmd_kind: u32,
            payload_ptr: *const u8, payload_len: u32,
        ) -> u32 {
            $crate::runtime::dispatch::<$ty>(
                dispatch_id, cmd_kind, payload_ptr, payload_len,
            )
        }

        #[no_mangle]
        pub extern "C" fn akiva_app_poll_dispatch(dispatch_id: u64) -> u64 {
            $crate::runtime::poll_dispatch(dispatch_id)
        }

        #[no_mangle]
        pub extern "C" fn akiva_app_get_result(
            dispatch_id: u64, out_len_ptr: *mut u32,
        ) -> *const u8 {
            $crate::runtime::get_result(dispatch_id, out_len_ptr)
        }

        /// Stone 11.6.5 (#11) — drive every `TaskScope`-spawned future
        /// across every live activity slot. Returns the count still
        /// pending after the sweep so the host can short-circuit when
        /// idle. Called by the worker shell on every `host_op_done`
        /// arrival; cheap when no spawns exist.
        #[no_mangle]
        pub extern "C" fn akiva_app_drive_spawned() -> u32 {
            $crate::runtime::drive_spawned()
        }
    };
}
