#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
exec mise exec -- uv run --no-project --python 3.12 scripts/update-transcribe.py "$@"
