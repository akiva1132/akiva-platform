//! Resource accessor — resolves `@string/name` and `@color/name`
//! references against the active package's values tables.
//!
//! Apps call these either directly (`res::color("button_bg")`) or
//! indirectly through the AKML inflater, which invokes them when it
//! sees a `@color/…` or `@string/…` attribute.
//!
//! The tables themselves live host-side: the framework parses each
//! package's `res/values/*.toml` at boot and the worker.js bridge
//! caches the same data in JS for fast per-call lookup. These
//! functions are just typed wrappers around two host imports that
//! hit the JS side directly; nothing round-trips through the kernel.
//!
//! ## Missing refs are visible, not silent
//!
//! A `@color/foo` that isn't registered resolves to magenta
//! (`0xFFFF00FF`) and triggers a host-side warn log. A `@string/bar`
//! that isn't registered resolves to the literal text
//! `⟨missing:bar⟩` — same log. The goal is to surface typos on the
//! first render instead of letting them hide as zero-alpha black or
//! empty strings. Apps that need to distinguish "defined but equal
//! to magenta" from "undefined" should call `contains_color` /
//! `contains_string` first.

/// Sentinel the host returns for an unknown `@color/` ref. Also the
/// value the SDK writes into the paint stream — so unresolved refs
/// show up on screen as loud magenta. Apps whose palette legitimately
/// includes this value should rename, since there's no way to
/// distinguish "missing" from "this exact colour" on the return.
pub const MISSING_COLOR: u32 = 0xFFFF00FF;

#[cfg(target_arch = "wasm32")]
extern "C" {
    fn akiva_host_resource_color(name_ptr: *const u8, name_len: u32) -> u32;
    fn akiva_host_resource_string(
        name_ptr: *const u8,
        name_len: u32,
        out_ptr: *mut u8,
        out_cap: u32,
    ) -> u32;
    fn akiva_host_resource_has_string(name_ptr: *const u8, name_len: u32) -> u32;
    fn akiva_host_resource_has_color(name_ptr: *const u8, name_len: u32) -> u32;
}

/// Resolve `@color/name` to its 0xAARRGGBB value. Returns
/// [`MISSING_COLOR`] (magenta) if the name isn't registered —
/// missing refs are a programming bug, and magenta is the Android
/// convention for "loud, visible, obviously broken".
#[inline]
pub fn color(name: &str) -> u32 {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_resource_color(name.as_ptr(), name.len() as u32)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
        MISSING_COLOR
    }
}

/// Resolve `@string/name` to its value. Returns `⟨missing:NAME⟩`
/// verbatim if the name isn't registered — loud, obvious, still
/// renderable, mirrors the [`color`] policy.
pub fn string(name: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        // Start with a buffer that fits most UI strings; if the host
        // reports a longer length, grow once and retry. Two round
        // trips max per lookup.
        let mut buf = vec![0u8; 128];
        let needed = unsafe {
            akiva_host_resource_string(
                name.as_ptr(), name.len() as u32,
                buf.as_mut_ptr(), buf.len() as u32,
            )
        };
        if (needed as usize) <= buf.len() {
            buf.truncate(needed as usize);
        } else {
            buf.resize(needed as usize, 0);
            let written = unsafe {
                akiva_host_resource_string(
                    name.as_ptr(), name.len() as u32,
                    buf.as_mut_ptr(), buf.len() as u32,
                )
            };
            buf.truncate(written as usize);
        }
        // Host-side tables are validated as UTF-8 at parse time, so
        // `from_utf8_lossy` is just a belt-and-braces safety net.
        String::from_utf8_lossy(&buf).into_owned()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        format!("⟨missing:{name}⟩")
    }
}

/// Probe: is `@color/name` registered? Used by call sites that need
/// to distinguish "undefined" from "happens to equal magenta".
pub fn contains_color(name: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_resource_has_color(name.as_ptr(), name.len() as u32) != 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
        false
    }
}

/// Probe: is `@string/name` registered?
pub fn contains_string(name: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        akiva_host_resource_has_string(name.as_ptr(), name.len() as u32) != 0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
        false
    }
}
