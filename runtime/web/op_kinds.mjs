// Op-kind constants for the unified ABI (Stone 11.3).
//
// JS mirror of `framework/src/wire/op_kinds.rs`. The two MUST stay
// numerically identical: an app's wasm bakes in the Rust-side
// constants when it calls `akiva_host_io_start(op_kind, ...)`, so a
// drift here means the dispatcher routes (or rejects) the wrong op.
// A Vitest test pins this — see the `op_kinds mirror` block in
// `__tests__/app_registry.test.mjs`.
//
// Numbering layout (locked):
//   FIRE   range: 0x0001..=0x00FF
//   IO     range: 0x0101..=0x01FF
//   reserved:    0x0200+   (don't allocate yet)
//
// Disjoint ranges let a corrupted args blob that picks an io kind
// from a fire import (or vice versa) be rejected loudly rather than
// silently mis-decoded.

export const FIRE = Object.freeze({
  LOG:                       0x0001,
  FINISH:                    0x0002,
  START_ACTIVITY:            0x0003,
  START_ACTIVITY_FOR_RESULT: 0x0004,
  SET_RESULT:                0x0005,
});

export const IO = Object.freeze({
  TEXT_WIDTH:     0x0101,
  QUERY_PACKAGES: 0x0102,
  STORAGE_GET:    0x0103,
  STORAGE_SET:    0x0104,
  STORAGE_DELETE: 0x0105,
  TIMER_SLEEP:    0x0106,  // Stone 13.0prep
});

// Stone 11.4b — host→app dispatch op kinds. Mirror of
// `framework::wire::op_kinds::cmd` and `akiva_sdk::op_kinds::cmd`.
// The host calls `akiva_app_dispatch(cmd_kind, payload_ptr,
// payload_len) -> u64` for every per-callback transition; the wasm
// app demuxes by `cmd_kind`. Result bytes (RENDER, SAVE_STATE) flow
// back via `akiva_app_get_result(future_id, out_len_ptr) -> *const u8`.
//
// Layout:
//   Lifecycle: 0x01..=0x0F   (Create has a class-string payload;
//                             Start/Resume/Pause/Stop/Destroy carry
//                             only the handle.)
//   Input:     0x10..=0x1F
//   Render:    0x20..=0x2F
//   State:     0x30..=0x3F
//   Intent:    0x40..=0x4F
//   reserved:  0x50+
export const CMD = Object.freeze({
  CREATE:          0x01,
  START:           0x02,
  RESUME:          0x03,
  PAUSE:           0x04,
  STOP:            0x05,
  DESTROY:         0x06,

  POINTER:         0x10,
  KEY:             0x11,
  RESIZE:          0x12,

  RENDER:          0x20,

  SAVE_STATE:      0x30,
  RESTORE_STATE:   0x31,

  NEW_INTENT:      0x40,
  ACTIVITY_RESULT: 0x41,
});

// `akiva_host_io_poll` packs status + length into a single u64:
//   low 32 bits  = status (PENDING / READY / ERROR)
//   high 32 bits = payload length (when READY or ERROR; 0 when PENDING)
//
// 0/1/2 are wire constants — apps check `>= 1` to mean resolved and
// `== 2` to mean error. Reordering breaks SDK glue.
export const STATUS = Object.freeze({
  PENDING: 0,
  READY:   1,
  ERROR:   2,
  // Stone 11.6.8 #2 — host backpressure. Returned by io_poll when the
  // host refused the original io_start because the app's pending
  // futures map is at MAX_PENDING_PER_SLOT. Apps see this as
  // `Err(_::Busy)` from the SDK; cooperative apps back off, hostile
  // ones simply hit the cap on every spam attempt.
  BUSY:    3,
});

/** Pack `(status, length)` into the u64 shape `akiva_host_io_poll` returns. */
export function packPoll(status, length) {
  return BigInt(status) | (BigInt(length) << 32n);
}

/** True iff `kind` is a valid fire op_kind in the locked range. */
export function isFireKind(kind) {
  return kind >= 0x0001 && kind <= 0x00FF;
}

/** True iff `kind` is a valid io op_kind in the locked range. */
export function isIoKind(kind) {
  return kind >= 0x0101 && kind <= 0x01FF;
}

/** True iff `kind` is a valid host→app dispatch cmd_kind. (Stone 11.4b) */
export function isCmdKind(kind) {
  return kind >= 0x01 && kind <= 0x4F;
}
