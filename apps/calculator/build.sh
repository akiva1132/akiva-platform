#!/usr/bin/env bash
# Build apps/calculator for wasm and stage it for the dev-mode install hook.
#
# Usage: apps/calculator/build.sh [--debug]
#
# Outputs:
#   runtime/web/dev-apps/calculator/app.wasm
#   runtime/web/dev-apps/calculator/manifest.toml

set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$APP_DIR/../.." && pwd)"
DEST_DIR="$REPO_ROOT/runtime/web/dev-apps/calculator"

PROFILE="release"
PROFILE_DIR="release"
if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="dev"
  PROFILE_DIR="debug"
fi

echo "[calculator] cargo build ($PROFILE)"
cd "$APP_DIR"
cargo build --target wasm32-unknown-unknown --profile "$PROFILE"

WASM_NAME="akiva_app_calculator.wasm"
WASM_PATH="$APP_DIR/target/wasm32-unknown-unknown/$PROFILE_DIR/$WASM_NAME"
if [[ ! -f "$WASM_PATH" ]]; then
  echo "[calculator] expected $WASM_PATH but it's missing" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
cp "$WASM_PATH" "$DEST_DIR/app.wasm"
cp "$APP_DIR/manifest.toml" "$DEST_DIR/manifest.toml"
echo "[calculator] staged $DEST_DIR/app.wasm ($(wc -c < "$DEST_DIR/app.wasm") bytes)"
