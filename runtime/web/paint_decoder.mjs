// Stone 13.4 — JS-side paint stream decoder.
//
// Mirror of `framework::wasm_activity::decode_op` for the follower-tab
// paint path. The kernel encodes per-display paint ops via
// `framework::wasm_activity::encode_op` and posts the bytes through
// `hal_post_to_display(displayId, ptr, len)`. The leader tab forwards
// the bytes to the claiming follower over the BroadcastChannel; the
// follower runs them through this decoder against its local
// Canvas2DBackend (main-thread, since the canvas was never
// transferred away from the follower's tab).
//
// Wire format (fixed, locked by the cross-crate test in
// `framework::wire::op_kinds`):
//   opcode (u8) + per-op payload
//
// Opcodes — see `framework/src/wasm_activity.rs::opcode`. Mirror the
// constants here; a regression that drifts is caught by visual diff
// rather than by a test (no Rust↔JS pin for the paint layer today).
const OP_CLEAR              = 0;
const OP_FILL_RECT          = 1;
const OP_DRAW_TEXT          = 2;
const OP_PUSH_CLIP          = 3;
const OP_POP_CLIP           = 4;
const OP_PUSH_TRANSFORM     = 5;
const OP_POP_TRANSFORM      = 6;
const OP_FILL_ROUND_RECT    = 7;
const OP_STROKE_RECT        = 8;
const OP_STROKE_ROUND_RECT  = 9;
const OP_DRAW_LINE          = 10;
const OP_DRAW_IMAGE         = 11;
const OP_DRAW_FOREIGN_IMAGE = 12;
const OP_SET_GLOBAL_ALPHA   = 13;
// Phase 15.1 — per-Activity surface compositor markers.
const OP_BEGIN_SURFACE      = 14;
const OP_END_SURFACE        = 15;
const OP_COMPOSITE_SURFACE  = 16;
const OP_DESTROY_SURFACE    = 17;

const decoder = new TextDecoder("utf-8");

/**
 * Decode `bytes` (Uint8Array of encoded paint ops) and apply each op
 * to `backend` — any object that implements the same surface as
 * `Canvas2DBackend` (clear / fillRect / fillRoundRect / drawText /
 * pushClip / popClip / pushTransform / popTransform / strokeRect /
 * strokeRoundRect / drawLine / drawImage / present).
 *
 * Unknown opcodes abort the decode (the wire format has no length
 * prefix per op, so we can't resync). The backend's `present()` is
 * called once at the end so the canvas flips synchronously like the
 * leader's own draw path does.
 *
 * @param {Uint8Array} bytes
 * @param {{
 *   clear(rgba: number): void,
 *   fillRect(x: number, y: number, w: number, h: number, rgba: number): void,
 *   fillRoundRect(x: number, y: number, w: number, h: number, r: number, rgba: number): void,
 *   drawText(x: number, y: number, size: number, rgba: number, text: string): void,
 *   pushClip(x: number, y: number, w: number, h: number): void,
 *   popClip(): void,
 *   pushTransform?(dx: number, dy: number, scale: number): void,
 *   popTransform?(): void,
 *   strokeRect(x: number, y: number, w: number, h: number, width: number, rgba: number): void,
 *   strokeRoundRect(x: number, y: number, w: number, h: number, r: number, width: number, rgba: number): void,
 *   drawLine(x0: number, y0: number, x1: number, y1: number, width: number, rgba: number): void,
 *   drawImage?(id: number, x: number, y: number, w: number, h: number): void,
 *   present?(): void,
 * }} backend
 * @returns {{ ops: number, error: string | null }}
 */
export function applyPaintStream(bytes, backend) {
  if (!(bytes instanceof Uint8Array) || bytes.length === 0) {
    backend.present?.();
    return { ops: 0, error: null };
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let p = 0;
  let opCount = 0;

  const readF32 = () => { const v = view.getFloat32(p, true); p += 4; return v; };
  const readU32 = () => { const v = view.getUint32(p, true); p += 4; return v; };

  while (p < bytes.length) {
    const op = view.getUint8(p); p += 1;
    switch (op) {
      case OP_CLEAR: {
        const rgba = readU32();
        backend.clear(rgba >>> 0);
        break;
      }
      case OP_FILL_RECT: {
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        const rgba = readU32();
        backend.fillRect(x, y, w, h, rgba >>> 0);
        break;
      }
      case OP_DRAW_TEXT: {
        const x = readF32(), y = readF32(), size = readF32();
        const rgba = readU32();
        const len = readU32();
        const text = decoder.decode(bytes.subarray(p, p + len));
        p += len;
        backend.drawText(x, y, size, rgba >>> 0, text);
        break;
      }
      case OP_PUSH_CLIP: {
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        backend.pushClip(x, y, w, h);
        break;
      }
      case OP_POP_CLIP: {
        backend.popClip();
        break;
      }
      case OP_PUSH_TRANSFORM: {
        const dx = readF32(), dy = readF32(), scale = readF32();
        backend.pushTransform?.(dx, dy, scale);
        break;
      }
      case OP_POP_TRANSFORM: {
        backend.popTransform?.();
        break;
      }
      case OP_FILL_ROUND_RECT: {
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        const r = readF32();
        const rgba = readU32();
        backend.fillRoundRect(x, y, w, h, r, rgba >>> 0);
        break;
      }
      case OP_STROKE_RECT: {
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        const width = readF32();
        const rgba = readU32();
        backend.strokeRect(x, y, w, h, width, rgba >>> 0);
        break;
      }
      case OP_STROKE_ROUND_RECT: {
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        const r = readF32(), width = readF32();
        const rgba = readU32();
        backend.strokeRoundRect(x, y, w, h, r, width, rgba >>> 0);
        break;
      }
      case OP_DRAW_LINE: {
        const x0 = readF32(), y0 = readF32(), x1 = readF32(), y1 = readF32();
        const width = readF32();
        const rgba = readU32();
        backend.drawLine(x0, y0, x1, y1, width, rgba >>> 0);
        break;
      }
      case OP_DRAW_IMAGE: {
        const id = readU32();
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        backend.drawImage?.(id, x, y, w, h);
        break;
      }
      case OP_DRAW_FOREIGN_IMAGE: {
        const pkgLen = view.getUint16(p, true); p += 2;
        const pkg = decoder.decode(bytes.subarray(p, p + pkgLen));
        p += pkgLen;
        const nameLen = view.getUint16(p, true); p += 2;
        const name = decoder.decode(bytes.subarray(p, p + nameLen));
        p += nameLen;
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        backend.drawForeignImage?.(pkg, name, x, y, w, h);
        break;
      }
      case OP_SET_GLOBAL_ALPHA: {
        const alpha = readF32();
        backend.setGlobalAlpha?.(alpha);
        break;
      }
      case OP_BEGIN_SURFACE: {
        const id = readU32();
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        backend.beginSurface?.(id, x, y, w, h);
        break;
      }
      case OP_END_SURFACE: {
        backend.endSurface?.();
        break;
      }
      case OP_COMPOSITE_SURFACE: {
        const id = readU32();
        const x = readF32(), y = readF32(), w = readF32(), h = readF32();
        const opacity = readF32();
        const tx = readF32(), ty = readF32(), scale = readF32();
        const backdropBlurRadius = readF32();
        const shadowOffsetX = readF32();
        const shadowOffsetY = readF32();
        const shadowBlurRadius = readF32();
        const shadowRgba = readU32();
        backend.compositeSurface?.(
          id, x, y, w, h, opacity, tx, ty, scale,
          backdropBlurRadius,
          shadowOffsetX, shadowOffsetY, shadowBlurRadius, shadowRgba,
        );
        break;
      }
      case OP_DESTROY_SURFACE: {
        const id = readU32();
        backend.destroySurfaceCanvas?.(id);
        break;
      }
      default:
        return { ops: opCount, error: `unknown opcode 0x${op.toString(16)} at byte ${p - 1}` };
    }
    opCount += 1;
  }
  backend.present?.();
  return { ops: opCount, error: null };
}
