#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-target}"

if [[ ! -d "$ROOT" ]]; then
  exit 0
fi

while IFS= read -r -d '' checksum; do
  lib_path="${checksum%.sum}.a"
  if [[ ! -f "$lib_path" ]]; then
    rm -f "$checksum"
  fi
done < <(find "$ROOT" -type f -name 'librusty_v8.sum' -print0)
