#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-target}"

if [[ ! -d "$ROOT" ]]; then
  exit 0
fi

set -f

patterns=(
  "*/build/v8-*"
  "*/.fingerprint/v8-*"
  "*/deps/libv8*.rlib"
  "*/deps/libv8*.rmeta"
  "*/deps/libv8*.d"
  "*/gn_out"
  "*/librusty_v8.a"
  "*/librusty_v8.lib"
  "*/librusty_v8.sum"
)

for pattern in "${patterns[@]}"; do
  find "$ROOT" -path "$pattern" -print -exec rm -rf {} +
done

set +f
