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

        // ── Lifecycle ──────────────────────────────────────────────────
        #[no_mangle]
        pub extern "C" fn akiva_app_create(handle: u32, class_ptr: *const u8, class_len: u32) {
            $crate::runtime::on_create::<$ty>(handle, class_ptr, class_len);
        }
        #[no_mangle]
        pub extern "C" fn akiva_app_start(handle: u32)   { $crate::runtime::on_start(handle);   }
        #[no_mangle]
        pub extern "C" fn akiva_app_resume(handle: u32)  { $crate::runtime::on_resume(handle);  }
        #[no_mangle]
        pub extern "C" fn akiva_app_pause(handle: u32)   { $crate::runtime::on_pause(handle);   }
        #[no_mangle]
        pub extern "C" fn akiva_app_stop(handle: u32)    { $crate::runtime::on_stop(handle);    }
        #[no_mangle]
        pub extern "C" fn akiva_app_destroy(handle: u32) { $crate::runtime::on_destroy(handle); }

        // ── Input ──────────────────────────────────────────────────────
        #[no_mangle]
        pub extern "C" fn akiva_app_pointer(
            handle: u32, x: f32, y: f32, button: u32, phase: u32, ts_ms: f64,
        ) {
            $crate::runtime::on_pointer(handle, x, y, button, phase, ts_ms);
        }
        #[no_mangle]
        pub extern "C" fn akiva_app_key(
            handle: u32, code: u32, modifiers: u32, phase: u32, ts_ms: f64,
        ) {
            $crate::runtime::on_key(handle, code, modifiers, phase, ts_ms);
        }
        #[no_mangle]
        pub extern "C" fn akiva_app_resize(
            handle: u32, width_px: u32, height_px: u32, density: f32,
        ) {
            $crate::runtime::on_resize(handle, width_px, height_px, density);
        }

        // ── Render ─────────────────────────────────────────────────────
        #[no_mangle]
        pub extern "C" fn akiva_app_render(
            handle: u32, ts_ms: f64, out_len_ptr: *mut u32,
        ) -> *const u8 {
            $crate::runtime::on_render(handle, ts_ms, out_len_ptr)
        }

        // ── Saved state ────────────────────────────────────────────────
        // Same buffer-lives-until-next-call contract as `akiva_app_render`;
        // see the wire format in [sdk-rust/src/activity.rs].
        #[no_mangle]
        pub extern "C" fn akiva_app_save_state(
            handle: u32, out_len_ptr: *mut u32,
        ) -> *const u8 {
            $crate::runtime::on_save_state(handle, out_len_ptr)
        }
        #[no_mangle]
        pub extern "C" fn akiva_app_restore_state(
            handle: u32, bytes_ptr: *const u8, bytes_len: u32,
        ) {
            $crate::runtime::on_restore_state(handle, bytes_ptr, bytes_len)
        }

        // ── Intent redelivery (Stone 9.4) ───────────────────────────────
        // Called when an already-existing activity (SingleTop reuse, or
        // Phase 9.2's SingleTask/SingleInstance) is re-launched via
        // start_activity. The handle stays the same; the intent blob
        // carries whatever the launcher sent. See docs/intents.md §8.
        #[no_mangle]
        pub extern "C" fn akiva_app_new_intent(
            handle: u32, bytes_ptr: *const u8, bytes_len: u32,
        ) {
            $crate::runtime::on_new_intent(handle, bytes_ptr, bytes_len)
        }

        // ── Activity result delivery (Stone 9.5) ────────────────────────
        // Fires on the caller when the callee it launched via
        // start_activity_for_result finishes. `request_code` is what the
        // caller passed in; the bytes encode an `ActivityResult` (i32
        // code + optional data blob). If the callee finishes without
        // calling set_result, the framework synthesises a CANCELED
        // result with empty data.
        #[no_mangle]
        pub extern "C" fn akiva_app_activity_result(
            handle: u32, request_code: u32, bytes_ptr: *const u8, bytes_len: u32,
        ) {
            $crate::runtime::on_activity_result(handle, request_code, bytes_ptr, bytes_len)
        }
    };
}
