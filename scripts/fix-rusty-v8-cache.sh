#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-target}"

if [[ ! -d "$ROOT" ]]; then
  exit 0
fi

PYTHON_BIN=""

download_and_unpack() {
  local url="$1"
  local dest="$2"

  if [[ -z "$PYTHON_BIN" ]]; then
    if command -v python3 >/dev/null 2>&1; then
      PYTHON_BIN="python3"
    elif command -v python >/dev/null 2>&1; then
      PYTHON_BIN="python"
    else
      return 1
    fi
  fi

  "$PYTHON_BIN" - "$url" "$dest" <<'PY'
import gzip
import os
import shutil
import sys
import urllib.request

def main():
    url, dest = sys.argv[1:]
    tmp_download = dest + ".download"
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    try:
        with urllib.request.urlopen(url) as response, open(tmp_download, "wb") as tmp:
            shutil.copyfileobj(response, tmp)
        with gzip.open(tmp_download, "rb") as src, open(dest, "wb") as dst:
            shutil.copyfileobj(src, dst)
    finally:
        if os.path.exists(tmp_download):
            os.remove(tmp_download)

if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"Failed to download rusty_v8 archive: {exc}", file=sys.stderr)
        sys.exit(1)
PY
}

while IFS= read -r -d '' checksum; do
  base="${checksum%.sum}"
  lib_path=""

  if [[ -f "${base}.a" ]]; then
    continue
  elif [[ -f "${base}.lib" ]]; then
    continue
  fi

  if ! url="$(<"$checksum")"; then
    rm -f "$checksum"
    continue
  fi

  if [[ -z "${url//[$'\t\r\n ']/}" ]]; then
    rm -f "$checksum"
    continue
  fi

  if [[ "$url" =~ \.lib\.gz$ ]]; then
    lib_path="${base}.lib"
  else
    lib_path="${base}.a"
  fi

  if download_and_unpack "$url" "$lib_path"; then
    echo "Rehydrated rusty_v8 archive: ${lib_path#"$ROOT/"}"
    printf '%s' "$url" >"$checksum"
  else
    echo "Removing stale checksum (download failed): ${checksum#"$ROOT/"}"
    rm -f "$checksum"
  fi
done < <(find "$ROOT" -type f -name 'librusty_v8.sum' -print0)
