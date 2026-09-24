import fcntl
import json
import os
import subprocess
import time

import pytest

from support.paths import CACHE_DIR, REPO_ROOT, TESTS_ROOT

NIX_BINARY = "/nix/var/nix/profiles/default/bin/nix"
MIX_USERS_GROUP = "mix-users"
NIX_CONF_DEST = "/etc/nix/nix.conf"

IMAGE_TAG = "mix-bootstrap-test:latest"
REMOTE_IMAGE = os.environ.get("MIX_TEST_IMAGE")


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
    containerfile = TESTS_ROOT / "Containerfile"
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


@pytest.fixture()
def container(container_image, mix_binary):
    name = _start_container(container_image)
    try:
        subprocess.run(["podman", "cp", str(mix_binary), f"{name}:/usr/local/bin/mix"], check=True)
        yield Container(name)
    finally:
        subprocess.run(["podman", "rm", "-f", name], capture_output=True)
