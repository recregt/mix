#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
MIX_UPDATE_NIX_FIXTURES=1 exec bash "$here/../e2e/run.sh" -k nix_contract
