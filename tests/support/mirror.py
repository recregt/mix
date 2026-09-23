import ast
import fcntl
import hashlib
import http.server
import pathlib
import re
import subprocess
import tempfile
import threading

import pytest

from support.paths import CACHE_DIR, REPO_ROOT

MIRROR_TEST_USERS = ("ciuser", "ciuser2")

INSTALL_TEST_PACKAGE = "hello"

# A package deliberately left out of the mirror's cache: installing it against that mirror can
# only be done by compiling it.
UNCACHED_TEST_PACKAGE = "cowsay"

NIX_ARGS = ["--extra-experimental-features", "nix-command flakes"]


def _pin(target: str) -> tuple[str, str]:
    pins_src = (REPO_ROOT / "crates/pins/src/lib.rs").read_text()
    block_match = re.search(
        rf'target:\s*"{re.escape(target)}"\s*,(.*?)\n\s*\}},',
        pins_src,
        re.S,
    )
    if not block_match:
        raise RuntimeError(f"no pin found for {target} in crates/pins")
    block = block_match.group(1)

    url_match = re.search(r'url:\s*"([^"]+)"', block)
    sha256_match = re.search(r'sha256:\s*"([0-9a-f]{64})"', block)
    if not url_match or not sha256_match:
        raise RuntimeError(f"could not parse url/sha256 for {target} in crates/pins")
    return url_match.group(1), sha256_match.group(1)


def _source_rev(const_name: str) -> str:
    pins_src = (REPO_ROOT / "crates/pins/src/lib.rs").read_text()
    match = re.search(rf'{const_name}:\s*&str\s*=\s*"([0-9a-f]{{40}})"', pins_src)
    if not match:
        raise RuntimeError(f"could not find {const_name} in crates/pins")
    return match.group(1)


def _nix_conf_content() -> str:
    models_src = (REPO_ROOT / "crates/core/src/models.rs").read_text()
    match = re.search(r'NIX_CONF:\s*&str\s*=\s*(".*?");', models_src, re.S)
    if not match:
        raise RuntimeError("could not find the NIX_CONF constant in models.rs")
    return ast.literal_eval(match.group(1))


NIX_CONF_CONTENT = _nix_conf_content()

NIX_URL, NIX_SHA256 = _pin("x86_64-linux")
NIX_FILENAME = NIX_URL.rsplit("/", 1)[-1]
NIXPKGS_REV = _source_rev("NIXPKGS_REV")
HOME_MANAGER_REV = _source_rev("HOME_MANAGER_REV")


@pytest.fixture(scope="session")
def nix_tarball():
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    dest = CACHE_DIR / NIX_FILENAME

    with open(CACHE_DIR / f"{NIX_FILENAME}.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not dest.exists():
            subprocess.run(["curl", "-fsSL", "-o", str(dest), NIX_URL], check=True)
        digest = hashlib.sha256(dest.read_bytes()).hexdigest()
        assert digest == NIX_SHA256, f"cached tarball does not match the pin in pins.rs: got {digest}"

    return dest


@pytest.fixture(scope="session")
def mirror_sources():
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    sources = {
        "nixpkgs": (
            f"https://github.com/NixOS/nixpkgs/archive/{NIXPKGS_REV}.tar.gz",
            CACHE_DIR / f"nixpkgs-{NIXPKGS_REV}.tar.gz",
        ),
        "home-manager": (
            f"https://github.com/nix-community/home-manager/archive/{HOME_MANAGER_REV}.tar.gz",
            CACHE_DIR / f"home-manager-{HOME_MANAGER_REV}.tar.gz",
        ),
    }
    for url, dest in sources.values():
        with open(CACHE_DIR / f"{dest.name}.lock", "w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if not dest.exists():
                subprocess.run(["curl", "-fsSL", "-o", str(dest), url], check=True)
    return CACHE_DIR


_MIRROR_FLAKE_NIX = """
{
  description = "mirror cache seed";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/__NIXPKGS_REV__";
    home-manager = {
      url = "github:nix-community/home-manager/__HOME_MANAGER_REV__";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = { nixpkgs, home-manager, ... }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      homeConfigurations."__USER__" = home-manager.lib.homeManagerConfiguration {
        inherit pkgs;
        modules = [ ./home.nix ];
      };
    };
}
"""

_MIRROR_HOME_NIX = """
{ pkgs, ... }:
{
  home.username = "__USER__";
  home.homeDirectory = "/home/__USER__";
  home.stateVersion = "24.05";
  home.packages = [ pkgs.git __EXTRA_PACKAGES__ ];
}
"""


@pytest.fixture(scope="session")
def mirror_cache(mirror_sources):
    cache_dir = CACHE_DIR / "cache"
    users = "-".join(MIRROR_TEST_USERS)
    marker = CACHE_DIR / f"cache-{NIXPKGS_REV}-{HOME_MANAGER_REV}-{users}.built"

    with open(CACHE_DIR / "mirror-cache.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not marker.exists():
            secret_key, public_key = _signing_key()
            store_paths = [
                _seed_activation_package(user, secret_key, cache_dir)
                for user in MIRROR_TEST_USERS
            ]
            store_paths.append(
                _seed_activation_package(
                    MIRROR_TEST_USERS[0],
                    secret_key,
                    cache_dir,
                    extra_packages=[INSTALL_TEST_PACKAGE],
                )
            )
            (cache_dir / "mix-mirror.pub").write_text(public_key)
            marker.write_text("\n".join(store_paths))

    return cache_dir


def _signing_key() -> tuple[str, str]:
    secret_key = subprocess.run(
        ["nix", "key", "generate-secret", "--key-name", "mix-mirror-test", *NIX_ARGS],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    public_key = subprocess.run(
        ["nix", "key", "convert-secret-to-public", *NIX_ARGS],
        input=secret_key,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    return secret_key, public_key


def _seed_activation_package(
    user: str,
    secret_key: str,
    cache_dir: pathlib.Path,
    extra_packages: list[str] | None = None,
) -> str:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        flake_nix = (
            _MIRROR_FLAKE_NIX.replace("__NIXPKGS_REV__", NIXPKGS_REV)
            .replace("__HOME_MANAGER_REV__", HOME_MANAGER_REV)
            .replace("__USER__", user)
        )
        (tmp_path / "flake.nix").write_text(flake_nix)
        extra = " ".join(f"pkgs.{pkg}" for pkg in extra_packages or [])
        home_nix = _MIRROR_HOME_NIX.replace("__USER__", user).replace(
            "__EXTRA_PACKAGES__", extra
        )
        (tmp_path / "home.nix").write_text(home_nix)

        store_path = subprocess.run(
            [
                "nix",
                "build",
                f'path:{tmp_path}#homeConfigurations."{user}".activationPackage',
                "--no-link",
                "--print-out-paths",
                *NIX_ARGS,
            ],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

        deriver = subprocess.run(
            ["nix-store", "--query", "--deriver", store_path],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

        build_closure = subprocess.run(
            ["nix-store", "--query", "--requisites", "--include-outputs", deriver],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.split()

        secret_key_path = tmp_path / "mirror-signing-key"
        secret_key_path.write_text(secret_key)
        subprocess.run(
            [
                "nix",
                "store",
                "sign",
                "--key-file",
                str(secret_key_path),
                *build_closure,
                *NIX_ARGS,
            ],
            check=True,
        )

        cache_dir.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["nix", "copy", "--to", f"file://{cache_dir}", *build_closure, *NIX_ARGS],
            check=True,
        )
    return store_path


@pytest.fixture()
def mock_nix_server(nix_tarball):
    directory = str(nix_tarball.parent)

    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=directory, **kwargs)

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("0.0.0.0", 0), Handler)
    port = server.server_address[1]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield {"url": f"http://host.containers.internal:{port}"}
    finally:
        server.shutdown()


def mirror_args(mock_nix_server, mirror_cache):
    mirror_key = (mirror_cache / "mix-mirror.pub").read_text().strip()
    return ["--mirror", mock_nix_server["url"], "--mirror-key", mirror_key]


def bootstrap_root(container, mock_nix_server):
    """Bootstraps as bare root: the base runtime only, no per-user profile."""
    result = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])
    assert result.returncode == 0, result.stderr
    return result


def bootstrap_as(container, user: str, mock_nix_server, mirror_cache):
    """Bootstraps as an already-created sudo user, enrolling their home-manager profile."""
    result = container.exec(
        "mix",
        "bootstrap",
        *mirror_args(mock_nix_server, mirror_cache),
        user=user,
    )
    assert result.returncode == 0, result.stderr
    return result
