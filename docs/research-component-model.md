# WebAssembly Component Model — migration research

> **Research output for [v0.2-review.md](v0.2-review.md) Task 4.** Our ABI
> v0 is raw `extern "C"`; [`docs/abi.md §3`](abi.md) already sketches
> v1 as a Component Model target. The question is *when* to flip, not
> *whether* — but "when" is load-bearing because a premature cut costs
> far more than a deferred one.

Sources:
- [Component Model introduction](https://component-model.bytecodealliance.org/) — Bytecode Alliance.
- [State of WebAssembly 2026 (Dev Newsletter)](https://devnewsletter.com/p/state-of-webassembly-2026/) — ecosystem snapshot.
- [Platform Uno — State of WebAssembly 2025–2026](https://platform.uno/blog/the-state-of-webassembly-2025-2026/) — release timeline.
- [wit-bindgen on crates.io](https://crates.io/crates/wit-bindgen) — still 0.x as of the research cut.
- [Bytecode Alliance — tooling compatibility matrix](https://bytecodealliance.org/articles/component-model-tooling-compatibility) — official "technology preview" caveat.
- [jco on GitHub](https://github.com/bytecodealliance/jco) — 1.18.1 (April 2026), the current browser transpiler.

---

## 1. Status (April 2026)

| piece                                  | status                                                                    |
| -------------------------------------- | ------------------------------------------------------------------------- |
| Component Model itself                 | **shipped** with WASI Preview 2 (2025). WASI 0.3 Feb 2026; WASI 1.0 late 2026 / early 2027. |
| wit-bindgen (Rust bindings generator)  | **still 0.x.** Bytecode Alliance explicitly labels it a *technology preview* with a breaking-change policy. "Will announce when production-ready"; that announcement has **not** happened yet. |
| jco (browser transpiler)               | **1.18.1** (Apr 2026) — production-grade, stable API. Transpiles components to ESM + core wasm for Node/browser. |
| Browser runtime for Component Model    | **no native support.** Engines still only load core `.wasm`. Every browser app has to go through a jco transpile step. |
| Cross-language composition             | works well for Wasmtime-hosted servers. For browsers it's emulated via jco output glue. |

**Summary:** the *spec* is stable enough to design against (we already do, in `docs/abi.md §3-§5`). The *Rust toolchain* we'd need to build our SDK on top of (`wit-bindgen`) is not. The *browser delivery path* works but adds a transpile step to the app-build pipeline.

## 2. Does WIT express everything we need?

Our current ABI surface:

| thing                                       | WIT equivalent                             | cost to express                                     |
| ------------------------------------------- | ------------------------------------------ | --------------------------------------------------- |
| Lifecycle transitions (6 enum variants)     | `variant lifecycle { create, start, … }`   | trivial                                             |
| Pointer/key events (structs)                | `record pointer-event { … }`               | trivial                                             |
| `SurfaceMetrics`                            | `record metrics { … }`                     | trivial                                             |
| Paint-op stream (variable-length binary)    | `list<paint-op>` — each op as a `variant`  | **non-trivial.** Our current ~40 bytes/op packing becomes records with per-op tag overhead. Size-sensitive hot path. |
| `SavedState` (typed KV)                     | `list<tuple<string, saved-value>>`         | fine — not hot path                                 |
| Intent blob (Stone 9.4+)                    | `record intent { … }`                      | natural fit                                         |
| Resource strings (`@string/foo`)            | `interface resources { get-string: func(key: string) -> option<string> }` | natural fit |

**Verdict:** the administrative APIs (lifecycle, intents, state, resources) map cleanly. The **paint-op stream does not**. Re-representing 7 opcodes per frame × 60 fps as WIT records would bloat the per-frame binary and force extra serialization work on both sides. The narrow-binary hot path is worth preserving even if everything else moves to components.

## 3. Size overhead

Industry ballpark (no official number found): Component Model adds a runtime adapter layer worth ~10–30 % for small modules, less for larger ones. For our apps today:

| app        | current `.wasm` size (approx) | projected w/ Component Model | delta |
| ---------- | ----------------------------- | ---------------------------- | ----- |
| hello      | ~50 KB                        | ~60-80 KB                    | +20-60 % |
| calculator | ~300 KB                       | ~330-370 KB                  | +10-25 % |
| tic-tac-toe| ~250 KB                       | ~275-300 KB                  | +10-20 % |

Plus a jco-emitted JS shim per app (~10-20 KB) that the worker loads alongside the core wasm. Not prohibitive, but a measurable cost paid every time a user downloads an app, and the smaller the app, the bigger the relative hit.

## 4. Toolchain cost

Switching our SDK from raw `extern "C"` to `wit-bindgen`:

- **Build step added**: every SDK change (and every app) gains a `wit-bindgen rust …` pass in the build. `cargo` alone stops being the build entry. Our zero-dep CLI (Task 7) would have to drop the claim for apps that use the new SDK.
- **Control loss**: wit-bindgen picks the export names, the allocator, the string encoding. We currently hand-pick these (e.g. `akiva_app_alloc` returns `*mut u8`, the host reads `*out_len_ptr` in a specific shape). Breaking-change freedom disappears.
- **wit-bindgen 0.x churn risk**: the README explicitly warns of "possibly-API-breaking changes". We'd be pinning to a specific `0.X.Y` and dealing with migration on each bump, against a project whose own stated policy is "not production yet."

And the browser delivery step:

- **Jco in the toolchain**: `akiva package` would grow a jco transpilation pass. jco is 1.x and stable, but it's another dep and another ~40 MB of Node tooling for app authors. That's non-trivial for "I just want to write a calculator."

## 5. Migration options

### A) Cold cut (v0.3 or v0.4, big-bang)

Drop ABI v0 entirely, rebuild SDK on wit-bindgen, add jco to `akiva package`. Every existing app rebuilt.

- **Pros:** clean, single ABI, no technical debt.
- **Cons:** locks us to wit-bindgen's maturity cadence. If wit-bindgen breaks between SDK 0.x and 0.y, every app has to rebuild with our SDK bump. Paint-op hot-path regresses.

### B) Gradual (hybrid v0 + v1)

Keep v0 for the hot path (lifecycle, pointer/key/resize, render, save-state). Introduce components only for *new* APIs landing Stone 9.4+ (intents, results, notifications).

- **Pros:** low-risk first taste of wit-bindgen. Hot path untouched. If wit-bindgen breaks, only the new APIs are affected.
- **Cons:** two ABIs in the runtime forever, or until a cold cut anyway. Worker has to run two loaders. Higher ongoing complexity.

### C) Wait, re-evaluate at v0.4 (recommended)

Stay on v0 through v0.2 and v0.3. Revisit the decision when one of these lands:
- wit-bindgen 1.0 ships with a stability promise.
- An actual cross-language app lands (Go + Rust + AssemblyScript in one runtime).
- A WIT-native import we want badly lands in WASI 1.0.

By v0.4 the toolchain will be 12+ months older, breaking-change risk will have dropped, and we'll have real usage patterns to design against.

- **Pros:** zero toolchain cost today, narrow v0 ABI was designed to make the v1 switch cheap when we commit, paint-op path stays tight.
- **Cons:** we keep shipping a non-standard ABI. Non-Rust app authors need our bindings (we don't have any non-Rust authors).

## 6. Decision — **defer to v0.4+**

We stay on raw ABI v0 for v0.2 and v0.3. Three anchors for the re-evaluation:

1. **wit-bindgen announces production-ready (1.0).** Right now the README itself says "technology preview" — any migration built on it is fighting the toolchain's own stated maturity.
2. **A concrete app needs it.** No MVP app needs cross-language composition, streams, or any WIT-native API that v0 can't represent. "Might matter eventually" doesn't justify the toolchain cost now.
3. **Paint-op story.** Before migrating the render path, we need a Component-Model-friendly shape that doesn't balloon per-frame bytes. Might be `list<u8>` with app-side encoding (giving up the WIT record benefit for the hot path is arguably fine — it already works that way today).

None of these three will have resolved by v0.3. Checking again in the v0.4 review is the honest answer.

## 7. What to carry into v0.3 anyway

Even staying on v0, we can keep the v1 switch cheap:

- **Keep `docs/abi.md §1.2` narrow-by-design rule enforced.** Every new import/export gets the three-test gate (use case, cross-app justification, forward-compat mapping). Paint Phase 9 landings through that gate.
- **Track wit-bindgen releases.** When it hits 1.0, that's the signal to re-open this doc.
- **Don't let the SDK grow conveniences that won't survive a cold cut.** If wit-bindgen wouldn't generate it, don't add it to the manually-written side.

No ROADMAP changes. This research stands; the decision is "no switch now, re-check at v0.4."
