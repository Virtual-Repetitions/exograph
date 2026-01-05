#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-target}"

if [[ ! -d "$ROOT" ]]; then
  exit 0
fi

find "$ROOT" -type f -name 'librusty_v8.a' -print -delete || true
find "$ROOT" -type f -name 'librusty_v8.lib' -print -delete || true
find "$ROOT" -type f -name 'librusty_v8.sum' -print -delete || true
