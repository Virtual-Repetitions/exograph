#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-target}"

if [[ ! -d "$ROOT" ]]; then
  exit 0
fi

shopt -s globstar nullglob

remove_paths() {
  local pattern="$1"
  local matches=()
  while IFS= read -r path; do
    matches+=("$path")
  done < <(compgen -G "$pattern" || true)

  for path in "${matches[@]}"; do
    echo "$path"
    rm -rf "$path"
  done
}

patterns=(
  "${ROOT}/**/build/v8-*"
  "${ROOT}/**/.fingerprint/v8-*"
  "${ROOT}/**/deps/libv8*.rlib"
  "${ROOT}/**/deps/libv8*.rmeta"
  "${ROOT}/**/deps/libv8*.d"
  "${ROOT}/**/gn_out"
  "${ROOT}/**/librusty_v8.a"
  "${ROOT}/**/librusty_v8.lib"
  "${ROOT}/**/librusty_v8.sum"
)

for pattern in "${patterns[@]}"; do
  remove_paths "$pattern"
done
