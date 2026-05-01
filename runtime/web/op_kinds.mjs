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
