# runtime/web

JS HAL for Akiva. Loads `akiva-kernel.wasm` inside a Dedicated Worker and
exposes an input/output surface on the main thread.

## Layout

- `index.html` — page shell with canvas + boot banner.
- `tab.js` — per-tab script: leader election, canvas ownership, input forwarding.
- `leader.js` — Web Locks + BroadcastChannel election.
- `worker.js` — hosts the kernel wasm. Drives the frame loop.
- `renderer/canvas2d.js` — concrete renderer backend (Canvas2D).

## Serving

`worker.js` is loaded as a module. Serving requires HTTP with correct MIME
types. Any static file server works for Phase 1a; COOP/COEP headers are
NOT required until we introduce SharedArrayBuffer (Phase 2+).

```sh
# From the repo root:
python3 -m http.server 8765 --directory runtime/web
# then open http://localhost:8765/
```

For `akiva-kernel.wasm` to be found, build the kernel and copy the output
next to `index.html`:

```sh
cargo build-kernel
cp target/wasm32-unknown-unknown/release/akiva_kernel.wasm runtime/web/akiva-kernel.wasm
```

Phase 2 replaces this manual dance with `akiva devserver`.
