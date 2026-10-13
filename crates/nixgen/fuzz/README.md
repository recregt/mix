# Fuzz Testing `mix-nixgen`

This directory contains `cargo-fuzz` targets for `mix-nixgen`.

## Prerequisites

The repository's flake provides `cargo-fuzz` and the nightly compiler libFuzzer needs. Enter its shell from the repository root:

```bash
nix develop .#fuzz
```

Run the commands below from that shell. `verify-corpus` also calls `nix-instantiate` and `nix eval`, which come with Nix itself.

Without Nix, install `cargo-fuzz` and a nightly toolchain with `rustup`, and write `cargo +nightly fuzz` wherever this page says `cargo fuzz`.

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
cargo fuzz run parse
cargo fuzz run ident
cargo fuzz run attr-path
cargo fuzz run flake
cargo fuzz run home

# Faster run by reducing ASAN allocation overhead
ASAN_OPTIONS="malloc_context_size=0" cargo fuzz run parse

# Timed run (e.g., 1 hour with a 10s per-input timeout)
cargo fuzz run parse -- -max_total_time=3600 -timeout=10
```

## Running with `tmux` (kinda cool!)

Run all 5 targets concurrently across split panes in an isolated session:

```bash
tmux new-session -d -s fuzz "cargo fuzz run parse" \; \
  split-window -h "cargo fuzz run ident" \; \
  split-window -v "cargo fuzz run attr-path" \; \
  select-pane -t 0 \; \
  split-window -v "cargo fuzz run flake" \; \
  split-window -h "cargo fuzz run home" \; \
  attach
```

## Reproducing Crashes

If a crash is found, re-run against the crash artifact directly to get a full backtrace:

```bash
RUST_BACKTRACE=1 cargo fuzz run parse artifacts/parse/<crash-file>
```

## Coverage & Corpus Management

```bash
# Generate LLVM source-based code coverage from the corpus
cargo fuzz coverage parse

# Minimize corpus
cargo fuzz cmin parse

# Verify corpus parses with real Nix evaluator (requires nix CLI)
cargo run --release --bin verify-corpus -- corpus/parse
```

*(Note: If executing from `crates/nixgen` instead of `crates/nixgen/fuzz`, add `--manifest-path fuzz/Cargo.toml` and prefix paths with `fuzz/`)*
