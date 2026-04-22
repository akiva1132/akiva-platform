#!/usr/bin/env bash
# Build apps/hello for wasm and stage it for the dev-mode install hook.
#
# Usage: apps/hello/build.sh [--debug]
#
# Outputs:
#   runtime/web/dev-apps/hello/app.wasm
#   runtime/web/dev-apps/hello/manifest.toml
#
# The dev install hook in runtime/web/worker.js picks those up on boot
# and writes them into OPFS storage as package com.akiva.hello.

set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$APP_DIR/../.." && pwd)"
DEST_DIR="$REPO_ROOT/runtime/web/dev-apps/hello"

PROFILE="release"
PROFILE_DIR="release"
if [[ "${1:-}" == "--debug" ]]; then
  PROFILE="dev"
  PROFILE_DIR="debug"
fi

echo "[hello] cargo build ($PROFILE)"
cd "$APP_DIR"
cargo build --target wasm32-unknown-unknown --profile "$PROFILE"

WASM_NAME="akiva_app_hello.wasm"
WASM_PATH="$APP_DIR/target/wasm32-unknown-unknown/$PROFILE_DIR/$WASM_NAME"
if [[ ! -f "$WASM_PATH" ]]; then
  echo "[hello] expected $WASM_PATH but it's missing" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
cp "$WASM_PATH" "$DEST_DIR/app.wasm"
cp "$APP_DIR/manifest.toml" "$DEST_DIR/manifest.toml"

# Stage res/values/ if the app ships it.
if [[ -d "$APP_DIR/res/values" ]]; then
  mkdir -p "$DEST_DIR/res/values"
  rm -f "$DEST_DIR/res/values/strings.toml" "$DEST_DIR/res/values/colors.toml"
  for leaf in strings colors; do
    src="$APP_DIR/res/values/$leaf.toml"
    if [[ -f "$src" ]]; then
      cp "$src" "$DEST_DIR/res/values/$leaf.toml"
    fi
  done
fi

echo "[hello] staged $DEST_DIR/app.wasm ($(wc -c < "$DEST_DIR/app.wasm") bytes)"
