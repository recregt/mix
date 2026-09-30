import fcntl
import functools
import json
import os
import pathlib
import queue
import re
import subprocess
import threading
import time

import pytest

from support import mirror, resources
from support.paths import CACHE_DIR, REPO_ROOT, TESTS_ROOT

NIX_BINARY = "/nix/var/nix/profiles/default/bin/nix"
ERE_SPECIAL = set("\\.[]{}()*+?^$|")
MIX_USERS_GROUP = "mix-users"
NIX_CONF_DEST = "/etc/nix/nix.conf"

IMAGE_TAG = "mix-bootstrap-test:latest"
REMOTE_IMAGE = os.environ.get("MIX_TEST_IMAGE")
CONTAINER_NAME = re.compile(r"mix-test-(\d+)-\d+")
VERBOSITY = re.compile(r"-v+")
MIX_BINARIES = ("mix", "/usr/local/bin/mix")
SNAPSHOT_REPOSITORY = "mix-bootstrapped"
SNAPSHOT_TAG = re.compile(r"(\d+)-[0-9a-f]+")
SNAPSHOT_TEST = "snapshot::bootstrapped"
SNAPSHOT_LOCK = CACHE_DIR / "snapshot.lock"
BOOTSTRAPPED_USER = mirror.MIRROR_TEST_USERS[0]


def literal(text: str) -> str:
    return "".join(f"\\{char}" if char in ERE_SPECIAL else char for char in text)


class BackgroundProcess:
    def __init__(self, container: "Container", proc: subprocess.Popen, pattern: str):
        self.container = container
        self.proc = proc
        self.pattern = pattern
        self.output = ""
        self.lines: queue.Queue[str | None] = queue.Queue()
        threading.Thread(target=self._drain, daemon=True).start()

    def _drain(self) -> None:
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def _next_line(self, timeout: float) -> str | None:
        try:
            return self.lines.get(timeout=timeout)
        except queue.Empty:
            return None

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
            line = self._next_line(remaining)
            if line is None:
                break
            self.output += line
            if substring in line:
                return
        if self.proc.poll() is not None:
            raise AssertionError(
                f"process exited ({self.proc.returncode}) before printing output containing "
                f"{substring!r}:\n{self.output}"
            )
        raise TimeoutError(
            f"{substring!r} never appeared within {timeout:g}s; processes now:\n"
            f"{self.container.process_tree()}\noutput so far:\n{self.output}"
        )

    def signal(self, sig_name: str, timeout: float = 10.0) -> None:
        pid = self.pid(timeout=timeout)
        if self.container.exec("kill", f"-{sig_name}", pid).returncode != 0:
            ended = self.wait(timeout=timeout)
            raise AssertionError(
                f"process matching {self.pattern!r} ended before {sig_name} "
                f"(returncode={ended.returncode}):\n{ended.stdout}"
            )

    def wait(self, timeout: float = 30.0) -> subprocess.CompletedProcess:
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self._next_line(max(0.01, deadline - time.time()))
            if line is None:
                break
            self.output += line
        self.proc.wait(timeout=max(0.01, deadline - time.time()))
        return subprocess.CompletedProcess(self.proc.args, self.proc.returncode, self.output, "")


class Container:
    def __init__(self, name: str, verbatim: bool = False):
        self.name = name
        self.verbatim = verbatim

    def _traced(self, args: tuple) -> tuple:
        if self.verbatim:
            return args
        for index, arg in enumerate(args):
            if arg in MIX_BINARIES:
                rest = list(args[index + 1 :])
                options = 0
                while options < len(rest) and rest[options].startswith("-"):
                    options += 1
                kept = [arg for arg in rest[:options] if not VERBOSITY.fullmatch(arg)]
                return (*args[: index + 1], "-vvv", *kept, *rest[options:])
        return args

    def exec(self, *args, env=None, check=False, user=None):
        args = self._traced(args)
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
        args = self._traced(args)
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
        return BackgroundProcess(self, proc, pattern=literal(" ".join(args)))

    def process_tree(self) -> str:
        return self.exec("ps", "-eo", "pid,ppid,stat,wchan:24,etime,args", "--forest").stdout

    def path_exists(self, path: str) -> bool:
        return self.exec("test", "-e", path).returncode == 0


def _start_container(image: str, name: str, binary: pathlib.Path) -> str:
    subprocess.run(
        [
            "podman",
            "run",
            "-d",
            "--systemd=always",
            "--cap-add=SYS_ADMIN",
            "--pids-limit=-1",
            "--volume",
            f"{binary}:/usr/local/bin/mix:ro",
            "--name",
            name,
            image,
        ],
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


def reap_orphans() -> None:
    names = subprocess.run(
        ["podman", "ps", "-a", "--format", "{{.Names}}"], capture_output=True, text=True
    ).stdout.split()
    for name in names:
        match = CONTAINER_NAME.fullmatch(name)
        if match and not resources.alive(int(match.group(1))):
            subprocess.run(["podman", "rm", "-f", name], capture_output=True)
    tags = subprocess.run(
        ["podman", "images", "--filter", f"reference={SNAPSHOT_REPOSITORY}", "--format", "{{.Tag}}"],
        capture_output=True,
        text=True,
    ).stdout.split()
    for tag in tags:
        match = SNAPSHOT_TAG.fullmatch(tag)
        if match and not resources.alive(int(match.group(1))):
            subprocess.run(["podman", "rmi", "-f", f"{SNAPSHOT_REPOSITORY}:{tag}"], capture_output=True)


def snapshot_image() -> str:
    controller = os.environ["MIX_TEST_CONTROLLER_PID"]
    return f"{SNAPSHOT_REPOSITORY}:{controller}-{os.environ['MIX_TEST_SESSION']}"


def remove_snapshot() -> None:
    subprocess.run(["podman", "rmi", "-f", snapshot_image()], capture_output=True)


def _bootstrapped_image(container_image, mix_binary, mirror_server, cache) -> str:
    tag = snapshot_image()
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(SNAPSHOT_LOCK, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if subprocess.run(["podman", "image", "exists", tag]).returncode == 0:
            return tag
        name = f"mix-test-{os.getpid()}-{time.time_ns()}"
        waited = resources.admit(
            name, SNAPSHOT_TEST, resources.demand_for(SNAPSHOT_TEST, _known())
        )
        try:
            started = time.monotonic()
            _start_container(container_image, name, mix_binary)
            cgroup = _cgroup_of(name)
            resources.attach(name, cgroup)
            built = Container(name)
            create_user(built, BOOTSTRAPPED_USER, sudo=True)
            mirror.bootstrap_as(built, BOOTSTRAPPED_USER, mirror_server, cache)
            resources.record(SNAPSHOT_TEST, cgroup, time.monotonic() - started, waited, "build")
            subprocess.run(["podman", "stop", name], check=True, capture_output=True)
            subprocess.run(["podman", "commit", "--quiet", name, tag], check=True, capture_output=True)
        finally:
            subprocess.run(["podman", "rm", "-f", name], capture_output=True)
            resources.release(name)
    return tag


def _cgroup_of(name: str) -> pathlib.Path:
    path = subprocess.run(
        ["podman", "inspect", "-f", "{{.State.CgroupPath}}", name],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    return resources.CGROUP_ROOT / path.lstrip("/")


@functools.cache
def _known() -> dict[str, resources.Demand]:
    return resources.snapshotted(os.environ.get("MIX_TEST_SESSION", ""))


@pytest.fixture()
def container(request, container_image, mix_binary):
    test = request.node.nodeid
    bootstrapped = request.node.get_closest_marker("bootstrapped") is not None
    image = container_image
    if bootstrapped:
        image = _bootstrapped_image(
            container_image,
            mix_binary,
            request.getfixturevalue("mock_nix_server"),
            request.getfixturevalue("mirror_cache"),
        )
    name = f"mix-test-{os.getpid()}-{time.time_ns()}"
    waited = resources.admit(name, test, resources.demand_for(test, _known()))
    started = time.monotonic()
    cgroup = None
    try:
        _start_container(image, name, mix_binary)
        cgroup = _cgroup_of(name)
        resources.attach(name, cgroup)
        yield Container(name, verbatim=request.node.get_closest_marker("verbatim_output") is not None)
    finally:
        if cgroup is not None:
            variant = "snapshot" if bootstrapped else "fresh"
            resources.record(test, cgroup, time.monotonic() - started, waited, variant)
        subprocess.run(["podman", "rm", "-f", name], capture_output=True)
        resources.release(name)

def root_result(stdout: str) -> dict:
    """The root node's finish, read from `--output json`: the command's typed result and exit code."""
    envelopes = [json.loads(line) for line in stdout.splitlines() if line.strip()]
    (finished,) = [
        envelope["nodeFinished"]
        for envelope in envelopes
        if "nodeFinished" in envelope and envelope["nodeFinished"]["id"] == "1"
    ]
    return finished
