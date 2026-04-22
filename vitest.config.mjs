// Vitest config for the JS-side runtime tests.
//
// Scope is intentionally narrow: only the worker-side host code in
// `runtime/web/*.js`. Rust + wasm logic is covered by `cargo test`.
// Node environment is sufficient — AppRegistry's only browser dep is
// `WebAssembly.Memory`, which Node ships natively.
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["runtime/web/__tests__/**/*.test.mjs"],
    environment: "node",
  },
});
