#!/usr/bin/env python3
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

RULES = [
    (
        "mix-core",
        {
            "async-trait",
            "futures-util",
            "libc",
            "mio",
            "mix-exec",
            "mix-shell",
            "nix",
            "reqwest",
            "tokio",
            "tokio-util",
        },
        "does io or runs async code",
    ),
    (
        "mix-cli",
        {"mix-shell"},
        "is the privileged code the client reaches only through mix-rpc",
    ),
    (
        "mix-explain",
        {"mix-shell", "mix-rpc", "mix-exec", "tokio"},
        "is not words: mix-explain turns a fault into text and nothing else",
    ),
    (
        "mix-render",
        {"mix-shell", "mix-rpc", "mix-exec", "tokio"},
        "is not rendering: mix-render turns events into output and nothing else",
    ),
    *(
        (
            crate,
            {"mix-core", "mix-nixgen", "mix-pins"},
            "is the daemon's model: the client reads the protocol, not the core",
        )
        for crate in ("mix-cli", "mix-explain", "mix-render", "mix-ui")
    ),
]

DIRECT = {
    "mix-cli": (
        {
            "clap",
            "mix-events",
            "mix-render",
            "mix-rpc",
            "nix",
            "tokio",
            "uuid",
        },
        "is arguments, routing, transport and signals; anything else belongs in the crates it uses",
    ),
}


def host() -> str:
    version = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    return next(line.split()[1] for line in version.splitlines() if line.startswith("host:"))


def metadata() -> dict:
    return json.loads(
        subprocess.run(
            [
                "cargo",
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--filter-platform",
                host(),
                "--manifest-path",
                str(ROOT / "Cargo.toml"),
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )


def normal_closure(data: dict, start: str) -> set[str]:
    names = {package["id"]: package["name"] for package in data["packages"]}
    edges = {
        node["id"]: [
            dep["pkg"]
            for dep in node["deps"]
            if any(kind["kind"] is None for kind in dep["dep_kinds"])
        ]
        for node in data["resolve"]["nodes"]
    }
    root = next(id for id, name in names.items() if name == start and id in edges)
    seen, stack = set(), [root]
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        stack.extend(edges[current])
    seen.discard(root)
    return {names[id] for id in seen}


def normal_direct(data: dict, crate: str) -> set[str]:
    package = next(package for package in data["packages"] if package["name"] == crate)
    return {dep["name"] for dep in package["dependencies"] if dep["kind"] is None}


def main() -> int:
    data = metadata()
    failed = False
    for crate, forbidden, reason in RULES:
        for name in sorted(normal_closure(data, crate) & forbidden):
            print(f"{crate} depends on {name}, which {reason}", file=sys.stderr)
            failed = True
    for crate, (allowed, reason) in DIRECT.items():
        for name in sorted(normal_direct(data, crate) - allowed):
            print(f"{crate} depends directly on {name}, but {crate} {reason}", file=sys.stderr)
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
