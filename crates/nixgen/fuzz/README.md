# Fuzz Testing `mix-nixgen`

This directory contains `cargo-fuzz` targets for `mix-nixgen`.

## Prerequisites

You can bootstrap the entire toolchain using `mix`:

```bash
# Bootstrap Nix on the host (provides `nix` and `nix-instantiate`)
mix bootstrap

# Install fuzzing tools via mix
mix install cargo-fuzz rustup tmux

# Add the nightly compiler required by libFuzzer
rustup toolchain install nightly

```

Alternatively, install `cargo-fuzz` and `rustup` via your system package manager.

## Targets

* `parse`: Stresses AST parsing, checks for recursion limits/slice overflows, and enforces roundtrip normalization consistency.
* `ident`: Validates string identifiers, path sanitization, and invariant rejection (`Ident`, `FileName`, `Key`, `RelPath`).
* `attr-path`: Unfiltered testing of attribute path construction, encoding, and URI resolution.
* `flake`: Tests flake configuration generation and verifies syntax validity against template injection attacks.
* `home`: Tests `HomeModule` configuration and arbitrary package injection semantics.

## Running

Run targets from `crates/nixgen/fuzz`:

```bash
# Interactive run (single target)
cargo +nightly fuzz run parse
cargo +nightly fuzz run ident
cargo +nightly fuzz run attr-path
cargo +nightly fuzz run flake
cargo +nightly fuzz run home

# Faster run by reducing ASAN allocation overhead
ASAN_OPTIONS="malloc_context_size=0" cargo +nightly fuzz run parse

# Timed run (e.g., 1 hour with a 10s per-input timeout)
cargo +nightly fuzz run parse -- -max_total_time=3600 -timeout=10
```

## Running with `tmux` (kinda cool!)

Run all 5 targets concurrently across split panes in an isolated session:

```bash
tmux new-session -d -s fuzz "cargo +nightly fuzz run parse" \; \
  split-window -h "cargo +nightly fuzz run ident" \; \
  split-window -v "cargo +nightly fuzz run attr-path" \; \
  select-pane -t 0 \; \
  split-window -v "cargo +nightly fuzz run flake" \; \
  split-window -h "cargo +nightly fuzz run home" \; \
  attach
```

## Reproducing Crashes

If a crash is found, re-run against the crash artifact directly to get a full backtrace:

```bash
RUST_BACKTRACE=1 cargo +nightly fuzz run parse artifacts/parse/<crash-file>
```

## Coverage & Corpus Management

```bash
# Generate LLVM source-based code coverage from the corpus
cargo +nightly fuzz coverage parse

# Minimize corpus
cargo +nightly fuzz cmin parse

# Verify corpus parses with real Nix evaluator (requires nix CLI)
cargo run --release --bin verify-corpus -- corpus/parse
```

*(Note: If executing from `crates/nixgen` instead of `crates/nixgen/fuzz`, add `--manifest-path fuzz/Cargo.toml` and prefix paths with `fuzz/`)*
