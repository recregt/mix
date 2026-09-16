import ast
import fcntl
import hashlib
import http.server
import json
import os
import pathlib
import re
import subprocess
import tempfile
import threading
import time

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
CACHE_DIR = pathlib.Path(os.environ.get("XDG_CACHE_HOME", pathlib.Path.home() / ".cache")) / "mix-bootstrap-tests"
IMAGE_TAG = "mix-bootstrap-test:latest"
REMOTE_IMAGE = os.environ.get("MIX_TEST_IMAGE")
MIRROR_TEST_USERS = ("ciuser", "ciuser2")


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
NIX_CONF_DEST = "/etc/nix/nix.conf"
NIX_BINARY = "/nix/var/nix/profiles/default/bin/nix"
MIX_USERS_GROUP = "mix-users"

NIX_URL, NIX_SHA256 = _pin("x86_64-linux")
NIX_FILENAME = NIX_URL.rsplit("/", 1)[-1]
NIXPKGS_REV = _source_rev("NIXPKGS_REV")
HOME_MANAGER_REV = _source_rev("HOME_MANAGER_REV")


@pytest.fixture(scope="session")
def mix_binary():
    subprocess.run(
        ["cargo", "build", "--release", "-p", "mix-bin"],
        cwd=REPO_ROOT,
        check=True,
    )
    return REPO_ROOT / "target/release/mix"


@pytest.fixture(scope="session")
def container_image():
    containerfile = pathlib.Path(__file__).parent / "Containerfile"
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(CACHE_DIR / "container-image.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if REMOTE_IMAGE:
            pull = subprocess.run(["podman", "pull", REMOTE_IMAGE])
            if pull.returncode == 0:
                return REMOTE_IMAGE
        subprocess.run(
            ["podman", "build", "-q", "-t", IMAGE_TAG, "-f", str(containerfile), str(containerfile.parent)],
            check=True,
        )
    return IMAGE_TAG


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

NIX_ARGS = ["--extra-experimental-features", "nix-command flakes"]

_MIRROR_HOME_NIX = """
{ pkgs, ... }:
{
  home.username = "__USER__";
  home.homeDirectory = "/home/__USER__";
  home.stateVersion = "24.05";
  home.packages = [ pkgs.git ];
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


def _seed_activation_package(user: str, secret_key: str, cache_dir: pathlib.Path) -> str:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        flake_nix = (
            _MIRROR_FLAKE_NIX.replace("__NIXPKGS_REV__", NIXPKGS_REV)
            .replace("__HOME_MANAGER_REV__", HOME_MANAGER_REV)
            .replace("__USER__", user)
        )
        (tmp_path / "flake.nix").write_text(flake_nix)
        (tmp_path / "home.nix").write_text(_MIRROR_HOME_NIX.replace("__USER__", user))

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


class BackgroundProcess:
    def __init__(self, container: "Container", proc: subprocess.Popen, pattern: str):
        self.container = container
        self.proc = proc
        self.pattern = pattern
        self.output = ""

    def pid(self, timeout: float = 10.0) -> str:
        deadline = time.time() + timeout
        while time.time() < deadline:
            result = self.container.exec("pgrep", "-f", self.pattern)
            pids = [p for p in result.stdout.split() if p.isdigit()]
            if pids:
                return pids[0]
            if self.proc.poll() is not None:
                raise AssertionError(
                    f"process matching {self.pattern!r} exited before it could be found "
                    f"(returncode={self.proc.returncode})"
                )
            time.sleep(0.05)
        raise TimeoutError(f"process matching {self.pattern!r} never appeared in the container")

    def wait_for_output(self, substring: str, timeout: float = 15.0) -> None:
        deadline = time.time() + timeout
        while time.time() < deadline:
            remaining = max(0.01, deadline - time.time())
            line = _readline_with_timeout(self.proc.stdout, remaining)
            if line is None:
                raise AssertionError(
                    f"process exited before printing output containing {substring!r}: "
                    f"{self.output!r}"
                )
            self.output += line
            if substring in line:
                return
        raise TimeoutError(f"{substring!r} never appeared in output: {self.output!r}")

    def signal(self, sig_name: str, timeout: float = 10.0) -> None:
        pid = self.pid(timeout=timeout)
        self.container.exec("kill", f"-{sig_name}", pid, check=True)

    def wait(self, timeout: float = 30.0) -> subprocess.CompletedProcess:
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = _readline_with_timeout(self.proc.stdout, max(0.01, deadline - time.time()))
            if line is None:
                break
            self.output += line
        self.proc.wait(timeout=max(0.01, deadline - time.time()))
        return subprocess.CompletedProcess(self.proc.args, self.proc.returncode, self.output, "")


def _readline_with_timeout(stream, timeout: float):
    import selectors

    sel = selectors.DefaultSelector()
    sel.register(stream, selectors.EVENT_READ)
    try:
        if not sel.select(timeout=timeout):
            return None
    finally:
        sel.close()
    line = stream.readline()
    return line if line else None


class Container:
    def __init__(self, name: str):
        self.name = name

    def exec(self, *args, env=None, check=False, user=None):
        cmd = ["podman", "exec"]
        for key, value in (env or {}).items():
            cmd += ["-e", f"{key}={value}"]
        if user:
            cmd += ["-u", user]
        cmd += [self.name, *args]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if check and result.returncode != 0:
            raise AssertionError(f"{args} failed ({result.returncode}): {result.stderr}")
        return result

    def start_background(self, *args, env=None, user=None) -> BackgroundProcess:
        cmd = ["podman", "exec"]
        for key, value in (env or {}).items():
            cmd += ["-e", f"{key}={value}"]
        if user:
            cmd += ["-u", user]
        cmd += [self.name, *args]
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        return BackgroundProcess(self, proc, pattern=" ".join(args))

    def path_exists(self, path: str) -> bool:
        return self.exec("test", "-e", path).returncode == 0


def _start_container(image: str) -> str:
    name = f"mix-test-{os.getpid()}-{time.time_ns()}"
    subprocess.run(
        ["podman", "run", "-d", "--systemd=always", "--cap-add=SYS_ADMIN", "--name", name, image],
        check=True,
    )
    for _ in range(30):
        probe = subprocess.run(
            ["podman", "exec", name, "systemctl", "is-system-running"],
            capture_output=True,
            text=True,
        )
        if probe.stdout.strip() in ("running", "degraded"):
            return name
        time.sleep(1)
    subprocess.run(["podman", "rm", "-f", name], capture_output=True)
    raise RuntimeError("container systemd never became ready")


@pytest.fixture()
def container(container_image, mix_binary):
    name = _start_container(container_image)
    try:
        subprocess.run(["podman", "cp", str(mix_binary), f"{name}:/usr/local/bin/mix"], check=True)
        yield Container(name)
    finally:
        subprocess.run(["podman", "rm", "-f", name], capture_output=True)


def create_user(container: Container, name: str, sudo: bool = False) -> None:
    container.exec("useradd", "--create-home", name, check=True)
    if sudo:
        container.exec(
            "bash",
            "-c",
            f"echo '{name} ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/{name}",
            check=True,
        )


def group_members(container: Container, group: str) -> list[str]:
    entry = container.exec("getent", "group", group, check=True).stdout.strip()
    members = entry.split(":")[3]
    return sorted(member for member in members.split(",") if member)


def daemon_trusts(container: Container, user: str) -> bool:
    """Asks the running nix-daemon whether it trusts `user`."""
    home = container.exec("getent", "passwd", user, check=True).stdout.split(":")[5]
    result = container.exec(
        NIX_BINARY,
        "store",
        "info",
        "--json",
        "--store",
        "daemon",
        user=user,
        env={"HOME": home},
        check=True,
    )
    return bool(json.loads(result.stdout)["trusted"])
