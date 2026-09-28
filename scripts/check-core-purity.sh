#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
allowlist="$root/crates/core/io-allowlist"
forbidden=(async-trait futures-util libc mio mix-exec mix-shell nix reqwest tokio tokio-util)

present=$(cargo tree --manifest-path "$root/Cargo.toml" -p mix-core -e normal --prefix none --format '{p}' | awk '{print $1}' | sort -u)
allowed=$(sort -u "$allowlist")

status=0
for name in "${forbidden[@]}"; do
    if grep -qx "$name" <<<"$present" && ! grep -qx "$name" <<<"$allowed"; then
        echo "mix-core depends on $name, which does io or runs async code" >&2
        status=1
    fi
done
while read -r name; do
    [[ -z "$name" ]] && continue
    if ! grep -qx "$name" <<<"$present"; then
        echo "$name no longer reaches mix-core: remove it from crates/core/io-allowlist" >&2
        status=1
    fi
done <<<"$allowed"
exit "$status"
