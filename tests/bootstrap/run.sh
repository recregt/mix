#!/usr/bin/env bash
set -euo pipefail
export PYTHONDONTWRITEBYTECODE=1
dir="$(cd "$(dirname "$0")" && pwd)"
pytest_cmd=(uv run --with-requirements "$dir/requirements.txt" pytest)

count=$("${pytest_cmd[@]}" "$dir" --collect-only -q 2>/dev/null | grep -c "::" || true)
workers=$(( count < $(nproc) ? count : $(nproc) ))
[ "$workers" -lt 1 ] && workers=1

exec "${pytest_cmd[@]}" -n "$workers" "$dir" "$@"
