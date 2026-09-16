#!/usr/bin/env bash
set -euo pipefail

for tool in podman uv nix; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        if [ -n "${CI:-}" ]; then
            echo "$tool not found; required in CI" >&2
            exit 1
        fi
        echo "$tool not found; skipping bootstrap integration tests" >&2
        exit 0
    fi
done

export PYTHONDONTWRITEBYTECODE=1
dir="$(cd "$(dirname "$0")" && pwd)"
pytest_cmd=(uv run --with-requirements "$dir/requirements.txt" pytest)

count=$(grep -rh "^def test_" "$dir"/test_*.py | wc -l)
cap="${MIX_TEST_WORKERS:-$(( $(nproc) / 2 ))}"
workers=$(( count < cap ? count : cap ))
[ "$workers" -lt 1 ] && workers=1

exec "${pytest_cmd[@]}" -n "$workers" "$dir" "$@"
