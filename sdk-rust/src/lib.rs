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
pub mod intent;
pub mod log;
pub mod op_kinds;
pub mod pkgs;
pub mod res;
pub mod result;
pub mod runtime;
pub mod storage;
pub mod ui;

pub use activity::{
    block_on_ready, Activity, ActivityContext, Context, KeyEvent, KeyPhase, Metrics,
    PointerEvent, PointerPhase, SavedState, TaskScope,
};
pub use app::{App, AppContext};
pub use canvas::Canvas;
pub use intent::{Intent, IntentBuilder};
pub use result::{ActivityResult, RESULT_CANCELED, RESULT_OK};

/// Everything a minimal app needs with one `use`.
pub mod prelude {
    pub use crate::activity::{
        Activity, Context, KeyEvent, KeyPhase, Metrics, PointerEvent, PointerPhase, SavedState,
        TaskScope,
    };
    pub use crate::canvas::Canvas;
    pub use crate::intent::{Intent, IntentBuilder};
    pub use crate::result::{ActivityResult, RESULT_CANCELED, RESULT_OK};
    pub use crate::ui::{CrossAxis, MainAxis, Ui, UiState};
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

        // ── Unified host→app dispatch (Stone 11.4b) ─────────────────────
        // The host packs per-callback args into the payload (every
        // payload starts with `[handle: u32 LE, ...]`) and the app
        // demuxes by `cmd_kind`. See docs/abi.md §2.2.2 for the wire.
        //
        // Return value:
        //   `0`             — sync, no bytes to fetch.
        //   `handle as u64` — sync, RENDER/SAVE_STATE bytes available;
        //                     host calls `akiva_app_get_result(this, ...)`.
        //
        // 11.4c will introduce non-handle future_ids when the dispatch
        // genuinely defers (worker round-trip).
        #[no_mangle]
        pub extern "C" fn akiva_app_dispatch(
            cmd_kind: u32, payload_ptr: *const u8, payload_len: u32,
        ) -> u64 {
            $crate::runtime::dispatch::<$ty>(cmd_kind, payload_ptr, payload_len)
        }

        // Buffer-lifetime contract: pointer stays valid until the next
        // data-producing dispatch for the same handle.
        #[no_mangle]
        pub extern "C" fn akiva_app_get_result(
            future_id: u64, out_len_ptr: *mut u32,
        ) -> *const u8 {
            $crate::runtime::get_result(future_id, out_len_ptr)
        }
    };
}
