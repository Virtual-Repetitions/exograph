#!/usr/bin/env sh
set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

(
  cd "${SCRIPT_DIR}/../src/wasm-source"
  CARGO_TARGET_DIR="${PWD}/target" cargo build --target wasm32-unknown-unknown
)
