import fcntl
import hashlib
import http.server
import os
import pathlib
import re
import subprocess
import threading
import time

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
CACHE_DIR = pathlib.Path(os.environ.get("XDG_CACHE_HOME", pathlib.Path.home() / ".cache")) / "mix-bootstrap-tests"
IMAGE_TAG = "mix-bootstrap-test:latest"
REMOTE_IMAGE = os.environ.get("MIX_TEST_IMAGE")


def _pin(target: str) -> tuple[str, str]:
    pins_src = (REPO_ROOT / "crates/bootstrap/src/pins.rs").read_text()
    block_match = re.search(
        rf'target:\s*"{re.escape(target)}"\s*,(.*?)\n\s*\}},',
        pins_src,
        re.S,
    )
    if not block_match:
        raise RuntimeError(f"no pin found for {target} in pins.rs")
    block = block_match.group(1)

    url_match = re.search(r'url:\s*"([^"]+)"', block)
    sha256_match = re.search(r'sha256:\s*"([0-9a-f]{64})"', block)
    if not url_match or not sha256_match:
        raise RuntimeError(f"could not parse url/sha256 for {target} in pins.rs")
    return url_match.group(1), sha256_match.group(1)


NIX_URL, NIX_SHA256 = _pin("x86_64-linux")
NIX_FILENAME = NIX_URL.rsplit("/", 1)[-1]


@pytest.fixture(scope="session")
def mix_binary():
    subprocess.run(
        ["cargo", "build", "--release", "-p", "mix-app"],
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
