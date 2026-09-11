# mix

[![CodSpeed](https://img.shields.io/endpoint?url=https://codspeed.io/badge.json)](https://app.codspeed.io/recregt/mix?utm_source=badge)

Reproducible systems, made effortless.

`mix` provisions and repairs a managed Nix runtime on standard Linux
distributions: it fetches a pinned, checksum-verified archive, unpacks it,
creates the build users and groups, and wires up the `nix-daemon` service.

## Usage

```sh
sudo mix bootstrap     # initialize the runtime and system dependencies
sudo mix doctor        # inspect system health
sudo mix doctor --fix  # restore the managed state to a pristine condition
```

Both commands accept `--mirror <URL>` (or `MIX_NIX_MIRROR`) to fetch the pinned
archive from an internal mirror.

## Development

```sh
cargo test --workspace --locked                   # unit and integration tests
cargo clippy --workspace --all-targets --locked   # lints
cargo bench --workspace                           # benchmarks
```

Benchmarks are written with [divan](https://github.com/nvzqz/divan) and run on
every pull request through [CodSpeed](https://app.codspeed.io/recregt/mix), so
regressions in archive verification, extraction, and step orchestration are
caught before they land.

## License

[MPL-2.0](LICENSE)
