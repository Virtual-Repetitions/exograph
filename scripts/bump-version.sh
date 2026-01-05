#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(dirname "$SCRIPT_DIR")

cd "$REPO_ROOT"

if [ ! -f "Cargo.toml" ]; then
  echo "Error: Could not find Cargo.toml in repository root." 1>&2
  exit 1
fi

if [ $# -ne 1 ]; then
  echo "Usage: $0 <major|minor|patch>" 1>&2
  exit 1
fi

BUMP_TYPE=$1
case "$BUMP_TYPE" in
  major|minor|patch) ;;
  *)
    echo "Error: Bump type must be 'major', 'minor', or 'patch'." 1>&2
    exit 1
    ;;
esac

CURRENT_VERSION=$(grep -E '^[[:space:]]*version = "[0-9]+\.[0-9]+\.[0-9]+"' Cargo.toml | head -n 1 | sed -E 's/^[[:space:]]*version = "([0-9]+\.[0-9]+\.[0-9]+)"/\1/')
if [ -z "$CURRENT_VERSION" ]; then
  echo "Error: Unable to determine current version from Cargo.toml." 1>&2
  exit 1
fi

OLD_IFS=$IFS
IFS=.
set -- $CURRENT_VERSION
IFS=$OLD_IFS
MAJOR=$1
MINOR=$2
PATCH=$3

case "$BUMP_TYPE" in
  major)
    MAJOR=$((MAJOR + 1))
    MINOR=0
    PATCH=0
    ;;
  minor)
    MINOR=$((MINOR + 1))
    PATCH=0
    ;;
  patch)
    PATCH=$((PATCH + 1))
    ;;
esac

NEW_VERSION="${MAJOR}.${MINOR}.${PATCH}"

if [ "$CURRENT_VERSION" = "$NEW_VERSION" ]; then
  echo "Current version already matches desired version ($NEW_VERSION). Nothing to do."
  exit 0
fi

echo "Bumping Exograph version: $CURRENT_VERSION -> $NEW_VERSION"

python3 - "$CURRENT_VERSION" "$NEW_VERSION" <<'PY'
import sys
from pathlib import Path

old, new = sys.argv[1:]
cargo_toml = Path("Cargo.toml")

text = cargo_toml.read_text()
updated = text.replace(f'version = "{old}"', f'version = "{new}"', 1)

if text == updated:
    print("Error: failed to update Cargo.toml", file=sys.stderr)
    sys.exit(1)

cargo_toml.write_text(updated)
PY

cargo generate-lockfile >/dev/null

echo "Updated Cargo.toml and regenerated Cargo.lock."
echo "Next steps: review changes and run cargo build or tests as needed."
