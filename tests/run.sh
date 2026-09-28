#!/usr/bin/env bash
set -euo pipefail

for tool in podman uv nix; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        if [ -n "${CI:-}" ]; then
            echo "$tool not found; required in CI" >&2
            exit 1
        fi
        echo "$tool not found; skipping integration tests" >&2
        exit 0
    fi
done

export PYTHONDONTWRITEBYTECODE=1
dir="$(cd "$(dirname "$0")" && pwd)"
pytest_cmd=(uv run --project "$dir" --locked pytest)

exec "${pytest_cmd[@]}" -n "${MIX_TEST_WORKERS:-auto}" "$dir" "$@"
