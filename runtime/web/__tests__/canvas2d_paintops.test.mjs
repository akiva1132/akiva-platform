// Canvas2D backend — every PaintOp opcode reaches the canvas.
//
// Regression net: PushTransform / PopTransform / PushClip / PopClip
// shipped as no-ops in the kernel for months (`flush_primary` matched
// them but called no HAL imports), and Stone 13.3 animations only
// failed in production. This test asserts that every backend method
// produces a non-empty trace on the underlying CanvasRenderingContext2D
// — a method that silently does nothing fails here at PR time.
//
// Scope is the JS-side `Canvas2DBackend` only. Wire-format encode/decode
// is covered by `framework/src/wasm_activity.rs::tests`; the missing
// link this file plugs is "encoded op → ctx call". The kernel-side
// `flush_primary` mapping (PaintOp variant → `hal_draw_*` import) is
// trivial 1:1 and reads as a match arm — bugs there look like new
// variants without arms (compile error) or unreachable arms (clippy).

import { describe, it, expect, beforeEach } from "vitest";
import { Canvas2DBackend } from "../renderer/canvas2d.js";

/**
 * Mock CanvasRenderingContext2D that records every method call and
 * property write to a single ordered trace. Lets the assertions read
 * "this op produced exactly these primitive calls in this order"
 * instead of poking at individual spies.
 */
function makeMockContext() {
  const trace = [];
  const ctx = {
    set fillStyle(v)   { trace.push(["set:fillStyle", v]); },
    get fillStyle()    { return null; },
    set strokeStyle(v) { trace.push(["set:strokeStyle", v]); },
    get strokeStyle()  { return null; },
    set lineWidth(v)   { trace.push(["set:lineWidth", v]); },
    get lineWidth()    { return 1; },
    set lineCap(v)     { trace.push(["set:lineCap", v]); },
    get lineCap()      { return "butt"; },
    set font(v)        { trace.push(["set:font", v]); },
    get font()         { return ""; },
    set textBaseline(v){ trace.push(["set:textBaseline", v]); },
    get textBaseline() { return "top"; },
    set imageSmoothingEnabled(v) { trace.push(["set:imageSmoothingEnabled", v]); },
    get imageSmoothingEnabled() { return true; },
    set globalAlpha(v) { trace.push(["set:globalAlpha", v]); },
    get globalAlpha()  { return 1; },

    fillRect:   (...a) => trace.push(["fillRect",   ...a]),
    strokeRect: (...a) => trace.push(["strokeRect", ...a]),
    fillText:   (...a) => trace.push(["fillText",   ...a]),
    beginPath:  ()     => trace.push(["beginPath"]),
    roundRect:  (...a) => trace.push(["roundRect",  ...a]),
    moveTo:     (...a) => trace.push(["moveTo",     ...a]),
    lineTo:     (...a) => trace.push(["lineTo",     ...a]),
    fill:       ()     => trace.push(["fill"]),
    stroke:     ()     => trace.push(["stroke"]),
    save:       ()     => trace.push(["save"]),
    restore:    ()     => trace.push(["restore"]),
    translate:  (...a) => trace.push(["translate",  ...a]),
    scale:      (...a) => trace.push(["scale",      ...a]),
    rect:       (...a) => trace.push(["rect",       ...a]),
    clip:       ()     => trace.push(["clip"]),
    drawImage:  (...a) => trace.push(["drawImage",  a[0], ...a.slice(1)]),
  };
  return { ctx, trace };
}

/** Mock OffscreenCanvas-shaped surface returning a tracked 2d context. */
function makeMockSurface(width = 400, height = 300) {
  const { ctx, trace } = makeMockContext();
  const surface = {
    width, height,
    getContext: (type) => (type === "2d" ? ctx : null),
  };
  return { surface, ctx, trace };
}

/** Filter init-time noise so the per-op traces stay readable. */
function tail(trace, mark) {
  const idx = trace.indexOf(mark);
  return idx === -1 ? trace : trace.slice(idx + 1);
}

describe("Canvas2DBackend — every paint op reaches the canvas", () => {
  let backend, ctx, trace;

  beforeEach(() => {
    backend = new Canvas2DBackend();
    const surface = makeMockSurface(400, 300);
    backend.init(surface.surface, { width_px: 400, height_px: 300, density: 1 });
    ctx = surface.ctx;
    trace = surface.trace;
    trace.push("---init---");
  });

  it("clear fills the entire canvas with the rgba colour", () => {
    backend.clear(0xff112233);
    const ops = tail(trace, "---init---");
    expect(ops).toEqual([
      ["set:fillStyle", "rgba(17,34,51,1)"],
      ["fillRect", 0, 0, 400, 300],
    ]);
  });

  it("fillRect emits one fillStyle + one fillRect", () => {
    backend.fillRect(10, 20, 100, 50, 0x80aabbcc);
    expect(tail(trace, "---init---")).toEqual([
      ["set:fillStyle", expect.stringMatching(/^rgba\(170,187,204/)],
      ["fillRect", 10, 20, 100, 50],
    ]);
  });

  it("fillRoundRect with non-zero radius uses the path API, not fillRect", () => {
    backend.fillRoundRect(10, 20, 100, 50, 8, 0xff223344);
    const ops = tail(trace, "---init---");
    expect(ops).toEqual([
      ["set:fillStyle", "rgba(34,51,68,1)"],
      ["beginPath"],
      ["roundRect", 10, 20, 100, 50, 8],
      ["fill"],
    ]);
  });

  it("fillRoundRect with radius=0 falls back to fillRect (no path)", () => {
    backend.fillRoundRect(10, 20, 100, 50, 0, 0xff223344);
    const ops = tail(trace, "---init---");
    expect(ops.find(o => o[0] === "beginPath")).toBeUndefined();
    expect(ops.find(o => o[0] === "fillRect")).toEqual(["fillRect", 10, 20, 100, 50]);
  });

  it("drawText sets font + fillStyle then fillText (in that order)", () => {
    backend.drawText(50, 60, 14, 0xffffffff, "hi");
    const ops = tail(trace, "---init---");
    // The font and colour must be set before fillText, otherwise the
    // glyph would render in the previous op's style.
    const fontIdx     = ops.findIndex(o => o[0] === "set:font");
    const fillStyleIdx = ops.findIndex(o => o[0] === "set:fillStyle");
    const fillTextIdx = ops.findIndex(o => o[0] === "fillText");
    expect(fontIdx).toBeGreaterThanOrEqual(0);
    expect(fillStyleIdx).toBeGreaterThanOrEqual(0);
    expect(fillTextIdx).toBeGreaterThan(fontIdx);
    expect(fillTextIdx).toBeGreaterThan(fillStyleIdx);
    expect(ops[fillTextIdx]).toEqual(["fillText", "hi", 50, 60]);
  });

  // ── Stone 13.0prep B — outline ops ────────────────────────────────

  it("strokeRect emits strokeStyle + lineWidth + strokeRect", () => {
    backend.strokeRect(10, 20, 100, 50, 2, 0xff334455);
    const ops = tail(trace, "---init---");
    expect(ops).toEqual([
      ["set:strokeStyle", "rgba(51,68,85,1)"],
      ["set:lineWidth", 2],
      ["strokeRect", 10, 20, 100, 50],
    ]);
  });

  it("strokeRoundRect with radius>0 uses path API + stroke", () => {
    backend.strokeRoundRect(10, 20, 100, 50, 8, 2, 0xff445566);
    const ops = tail(trace, "---init---");
    expect(ops).toEqual([
      ["set:strokeStyle", "rgba(68,85,102,1)"],
      ["set:lineWidth", 2],
      ["beginPath"],
      ["roundRect", 10, 20, 100, 50, 8],
      ["stroke"],
    ]);
  });

  it("strokeRoundRect with radius=0 falls back to strokeRect", () => {
    backend.strokeRoundRect(10, 20, 100, 50, 0, 2, 0xff445566);
    const ops = tail(trace, "---init---");
    expect(ops.find(o => o[0] === "beginPath")).toBeUndefined();
    expect(ops.find(o => o[0] === "strokeRect")).toEqual(["strokeRect", 10, 20, 100, 50]);
  });

  it("drawLine uses moveTo + lineTo + stroke with round caps", () => {
    backend.drawLine(0, 0, 100, 100, 3, 0xffaabbcc);
    const ops = tail(trace, "---init---");
    expect(ops).toEqual([
      ["set:strokeStyle", "rgba(170,187,204,1)"],
      ["set:lineWidth", 3],
      ["set:lineCap", "round"],
      ["beginPath"],
      ["moveTo", 0, 0],
      ["lineTo", 100, 100],
      ["stroke"],
    ]);
  });

  // ── Stone 13.3 — transform/clip stack (the regression we just fixed) ──

  it("pushTransform calls save + translate (and scale when scale != 1)", () => {
    backend.pushTransform(10, 20, 1);
    const ops1 = tail(trace, "---init---");
    expect(ops1).toEqual([["save"], ["translate", 10, 20]]);

    trace.length = 0;
    trace.push("---reset---");
    backend.pushTransform(0, 0, 2);
    const ops2 = tail(trace, "---reset---");
    expect(ops2).toEqual([["save"], ["scale", 2, 2]]);
  });

  it("pushTransform with no-op deltas still calls save (matched by popTransform)", () => {
    // Regression: an early version skipped save() when dx=dy=0 and
    // scale=1, which would unbalance subsequent popTransform calls
    // and corrupt the canvas state stack.
    backend.pushTransform(0, 0, 1);
    expect(tail(trace, "---init---")).toEqual([["save"]]);
  });

  it("popTransform restores the canvas state stack", () => {
    backend.popTransform();
    expect(tail(trace, "---init---")).toEqual([["restore"]]);
  });

  it("pushClip applies a rectangular clip via save + rect + clip", () => {
    backend.pushClip(10, 20, 100, 50);
    expect(tail(trace, "---init---")).toEqual([
      ["save"],
      ["beginPath"],
      ["rect", 10, 20, 100, 50],
      ["clip"],
    ]);
  });

  it("popClip restores via the canvas state stack", () => {
    backend.popClip();
    expect(tail(trace, "---init---")).toEqual([["restore"]]);
  });

  // ── Quick-win — animation routes alpha through globalAlpha ────────

  it("setGlobalAlpha forwards the value to ctx.globalAlpha", () => {
    backend.setGlobalAlpha(0.5);
    expect(tail(trace, "---init---")).toEqual([["set:globalAlpha", 0.5]]);
  });

  it("setGlobalAlpha clamps values outside [0, 1]", () => {
    // An animation that overshoots its window would otherwise wash
    // out (>1) or invert (<0) on some browsers.
    backend.setGlobalAlpha(1.5);
    backend.setGlobalAlpha(-0.2);
    const ops = tail(trace, "---init---");
    expect(ops).toEqual([
      ["set:globalAlpha", 1],
      ["set:globalAlpha", 0],
    ]);
  });

  it("a transition wrapper sets alpha inside a save/restore scope", () => {
    // The animation pipeline emits PushTransform → SetGlobalAlpha →
    // (activity ops) → PopTransform. Canvas2D's `restore()` pops the
    // entire state including globalAlpha, so a single PopTransform
    // both undoes the translate AND resets alpha to whatever the
    // outer surface had.
    backend.pushTransform(50, 0, 1);
    backend.setGlobalAlpha(0.4);
    backend.fillRect(0, 0, 10, 10, 0xffffffff);
    backend.popTransform();
    const ops = tail(trace, "---init---");
    // Expected sequence: save, translate, set:globalAlpha, fillStyle, fillRect, restore.
    expect(ops[0]).toEqual(["save"]);
    expect(ops[1]).toEqual(["translate", 50, 0]);
    expect(ops[2]).toEqual(["set:globalAlpha", 0.4]);
    expect(ops[ops.length - 1]).toEqual(["restore"]);
  });

  // ── Stone 13.0prep C — drawables ──────────────────────────────────

  it("drawImage with a registered drawable blits the bitmap", () => {
    const fakeBitmap = { _bitmap: true };
    backend.registerDrawable(7, fakeBitmap);
    backend.drawImage(7, 10, 20, 100, 50);
    expect(tail(trace, "---init---")).toEqual([
      ["drawImage", fakeBitmap, 10, 20, 100, 50],
    ]);
  });

  it("drawImage with id=0 paints the magenta missing-drawable placeholder", () => {
    backend.drawImage(0, 10, 20, 100, 50);
    expect(tail(trace, "---init---")).toEqual([
      ["set:fillStyle", "#ff00ff"],
      ["fillRect", 10, 20, 100, 50],
    ]);
  });

  it("drawImage with an unregistered id paints the magenta placeholder", () => {
    backend.drawImage(999, 10, 20, 100, 50);
    expect(tail(trace, "---init---")).toEqual([
      ["set:fillStyle", "#ff00ff"],
      ["fillRect", 10, 20, 100, 50],
    ]);
  });

  it("forgetDrawable evicts a previously registered bitmap", () => {
    backend.registerDrawable(7, { _bitmap: true });
    backend.forgetDrawable(7);
    backend.drawImage(7, 0, 0, 10, 10);
    // Reverts to magenta placeholder.
    const ops = tail(trace, "---init---");
    expect(ops[0]).toEqual(["set:fillStyle", "#ff00ff"]);
  });

  // ── Foundation gap — foreign-package drawables ───────────────────

  it("drawForeignImage hits the (pkg, name) cache when both match", () => {
    const fakeBitmap = { _bitmap: true };
    backend.registerForeignDrawable("com.calc", "launcher", fakeBitmap);
    backend.drawForeignImage("com.calc", "launcher", 10, 20, 48, 48);
    expect(tail(trace, "---init---")).toEqual([
      ["drawImage", fakeBitmap, 10, 20, 48, 48],
    ]);
  });

  it("drawForeignImage with unknown package paints magenta placeholder", () => {
    backend.registerForeignDrawable("com.calc", "launcher", { _b: 1 });
    backend.drawForeignImage("com.other", "launcher", 0, 0, 1, 1);
    expect(tail(trace, "---init---")).toEqual([
      ["set:fillStyle", "#ff00ff"],
      ["fillRect", 0, 0, 1, 1],
    ]);
  });

  it("drawForeignImage with unknown name in known package paints placeholder", () => {
    backend.registerForeignDrawable("com.calc", "launcher", { _b: 1 });
    backend.drawForeignImage("com.calc", "missing", 5, 5, 10, 10);
    expect(tail(trace, "---init---")).toEqual([
      ["set:fillStyle", "#ff00ff"],
      ["fillRect", 5, 5, 10, 10],
    ]);
  });

  it("drawForeignImage with empty name paints placeholder (no-icon package)", () => {
    // Empty name is the wire signal for "package has no app.icon".
    // The Shell can call drawForeignImage uniformly; the placeholder
    // surfaces the gap visually instead of crashing or reading a
    // wrong-package drawable.
    backend.drawForeignImage("com.calc", "", 0, 0, 10, 10);
    const ops = tail(trace, "---init---");
    expect(ops[0]).toEqual(["set:fillStyle", "#ff00ff"]);
  });

  it("forgetForeignDrawables evicts every entry for a package", () => {
    backend.registerForeignDrawable("com.calc", "launcher", { _b: 1 });
    backend.registerForeignDrawable("com.calc", "logo", { _b: 2 });
    backend.registerForeignDrawable("com.other", "launcher", { _b: 3 });
    backend.forgetForeignDrawables("com.calc");

    // Calc's drawables are gone — both names hit placeholder.
    backend.drawForeignImage("com.calc", "launcher", 0, 0, 1, 1);
    backend.drawForeignImage("com.calc", "logo", 0, 0, 1, 1);
    // Other package's entry survives.
    backend.drawForeignImage("com.other", "launcher", 0, 0, 1, 1);

    const ops = tail(trace, "---init---");
    // First two calls produced placeholders (set:fillStyle + fillRect each).
    expect(ops[0]).toEqual(["set:fillStyle", "#ff00ff"]);
    expect(ops[2]).toEqual(["set:fillStyle", "#ff00ff"]);
    // Third hit the cache.
    const lastOp = ops[ops.length - 1];
    expect(lastOp[0]).toBe("drawImage");
  });

  it("foreign and same-package caches are independent (no collision on flat id)", () => {
    // Two packages each register their own drawable_id=1 in the flat
    // cache (collision allowed at that level), and *also* in the
    // foreign cache keyed by (pkg, name). Foreign lookups must not
    // collide.
    const calcLauncher = { _b: "calc" };
    const shellHome    = { _b: "shell" };
    backend.registerDrawable(1, calcLauncher);
    backend.registerDrawable(1, shellHome);  // overwrites flat slot
    backend.registerForeignDrawable("com.calc", "launcher", calcLauncher);
    backend.registerForeignDrawable("sys.shell", "home", shellHome);

    // Flat path: only the most recent registration survived.
    backend.drawImage(1, 0, 0, 1, 1);
    // Foreign path: each package's entry resolves correctly.
    backend.drawForeignImage("com.calc", "launcher", 10, 0, 1, 1);
    backend.drawForeignImage("sys.shell", "home", 20, 0, 1, 1);

    const ops = tail(trace, "---init---");
    // First op is drawImage(shellHome, ...) (calc was overwritten).
    expect(ops[0]).toEqual(["drawImage", shellHome, 0, 0, 1, 1]);
    // Foreign hits resolve to the right bitmap each time.
    expect(ops[1]).toEqual(["drawImage", calcLauncher, 10, 0, 1, 1]);
    expect(ops[2]).toEqual(["drawImage", shellHome, 20, 0, 1, 1]);
  });

  // ── Sequencing tests — push/pop balance ─────────────────────────────

  it("a push/pop transform pair issues exactly one save + one restore", () => {
    backend.pushTransform(50, 0, 1);
    backend.fillRect(0, 0, 10, 10, 0xffffffff);
    backend.popTransform();
    const ops = tail(trace, "---init---");
    const saves    = ops.filter(o => o[0] === "save").length;
    const restores = ops.filter(o => o[0] === "restore").length;
    expect(saves).toBe(1);
    expect(restores).toBe(1);
  });

  it("a push/pop clip pair issues exactly one save + one restore", () => {
    backend.pushClip(0, 0, 100, 100);
    backend.fillRect(10, 10, 50, 50, 0xff000000);
    backend.popClip();
    const ops = tail(trace, "---init---");
    expect(ops.filter(o => o[0] === "save").length).toBe(1);
    expect(ops.filter(o => o[0] === "restore").length).toBe(1);
  });

  it("nested push/pop pairs preserve balance (transform inside clip)", () => {
    backend.pushClip(0, 0, 200, 200);
    backend.pushTransform(10, 0, 1);
    backend.fillRect(0, 0, 1, 1, 0xff000000);
    backend.popTransform();
    backend.popClip();
    const ops = tail(trace, "---init---");
    expect(ops.filter(o => o[0] === "save").length).toBe(2);
    expect(ops.filter(o => o[0] === "restore").length).toBe(2);
  });

  // ── No-context safety — every method tolerates an uninitialised backend ─

  it("methods called on an uninitialised backend are silent no-ops", () => {
    const fresh = new Canvas2DBackend();
    // None of these should throw.
    fresh.clear(0);
    fresh.fillRect(0, 0, 1, 1, 0);
    fresh.fillRoundRect(0, 0, 1, 1, 1, 0);
    fresh.drawText(0, 0, 12, 0, "x");
    fresh.strokeRect(0, 0, 1, 1, 1, 0);
    fresh.strokeRoundRect(0, 0, 1, 1, 1, 1, 0);
    fresh.drawLine(0, 0, 1, 1, 1, 0);
    fresh.pushTransform(0, 0, 1);
    fresh.popTransform();
    fresh.pushClip(0, 0, 1, 1);
    fresh.popClip();
    fresh.drawImage(0, 0, 0, 1, 1);
    fresh.drawForeignImage("com.x", "y", 0, 0, 1, 1);
    fresh.setGlobalAlpha(0.5);
    fresh.present();
  });
});

describe("rgbaToCss — colour packing matches HAL contract", () => {
  // The kernel hands us u32s as 0xAARRGGBB; canvas2d.js rgbaToCss is
  // private but its output is observable through any draw call. Test
  // it indirectly via fillRect with a few extreme values.

  let backend, trace;
  beforeEach(() => {
    backend = new Canvas2DBackend();
    const surface = makeMockSurface();
    backend.init(surface.surface, { width_px: 100, height_px: 100, density: 1 });
    trace = surface.trace;
    trace.push("---init---");
  });

  it("fully opaque red", () => {
    backend.fillRect(0, 0, 1, 1, 0xffff0000);
    const styleOp = tail(trace, "---init---").find(o => o[0] === "set:fillStyle");
    expect(styleOp[1]).toBe("rgba(255,0,0,1)");
  });

  it("half-alpha green", () => {
    backend.fillRect(0, 0, 1, 1, 0x8000ff00);
    const styleOp = tail(trace, "---init---").find(o => o[0] === "set:fillStyle");
    // 0x80 / 0xff ≈ 0.5019607...
    expect(styleOp[1]).toMatch(/^rgba\(0,255,0,0\.50/);
  });

  it("fully transparent blue", () => {
    backend.fillRect(0, 0, 1, 1, 0x000000ff);
    const styleOp = tail(trace, "---init---").find(o => o[0] === "set:fillStyle");
    expect(styleOp[1]).toBe("rgba(0,0,255,0)");
  });
});
