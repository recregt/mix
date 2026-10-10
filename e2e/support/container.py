import contextlib
import fcntl
import functools
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import threading
import time

import pytest

from support import mirror, resources, trace
from support.events import Run, document_of
from support.paths import CACHE_DIR, REPO_ROOT, TESTS_ROOT

NIX_BINARY = "/nix/var/nix/profiles/default/bin/nix"
MIX_USERS_GROUP = "mix-users"
NIX_CONF_DEST = "/etc/nix/nix.conf"

IMAGE_REPOSITORY = "mix-bootstrap-test"
REMOTE_IMAGE = os.environ.get("MIX_TEST_IMAGE")
CONTAINER_NAME = re.compile(r"mix-test-(\d+)-\d+")
SNAPSHOT_REPOSITORY = "mix-bootstrapped"
SNAPSHOT_TAG = re.compile(r"(\d+)-[0-9a-f]+")
SNAPSHOT_TEST = "snapshot::bootstrapped"
SNAPSHOT_LOCK = CACHE_DIR / "snapshot.lock"
BOOTSTRAPPED_USER = mirror.MIRROR_TEST_USERS[0]


def wait_any(*events: threading.Event) -> None:
    """Blocks until any of `events` is set."""
    any_set = threading.Event()
    for event in events:
        threading.Thread(target=lambda e=event: (e.wait(), any_set.set()), daemon=True).start()
    any_set.wait()


def until(wanted, what: str, unless=None):
    """Returns `wanted()` once it is truthy; fails as soon as `unless`, a background run or
    process, has ended instead."""
    ended = getattr(unless, "process", unless)
    while True:
        found = wanted()
        if found:
            return found
        if ended is not None and ended.ended.is_set():
            raise AssertionError(f"{what} never happened before {unless.wait()!r} ended")


class BackgroundProcess:
    """A program started in the container that the test waits for, without a deadline."""

    def __init__(self, container: Container, proc: subprocess.Popen, pidfile: str):
        self.container = container
        self.proc = proc
        self.pidfile = pidfile
        self.ended = threading.Event()
        self.stdout = ""
        self.stderr = ""
        threading.Thread(target=self._collect, daemon=True).start()

    def _collect(self) -> None:
        self.stdout, self.stderr = self.proc.communicate()
        self.ended.set()

    def pid(self) -> str:
        """The program's own pid in the container, which it wrote before it started."""
        while not self.ended.is_set():
            pid = self.container.exec("cat", self.pidfile).stdout.strip()
            if pid.isdigit():
                return pid
        raise AssertionError(
            f"{self.proc.args} exited ({self.proc.returncode}) before its pid was read:\n"
            f"{self.stdout}{self.stderr}"
        )

    def signal(self, sig_name: str) -> None:
        pid = self.pid()
        if self.container.exec("kill", f"-{sig_name}", pid).returncode != 0:
            self.ended.wait()
            raise AssertionError(
                f"{self.proc.args} ended before {sig_name} ({self.proc.returncode}):\n"
                f"{self.stdout}{self.stderr}"
            )

    def wait(self) -> subprocess.CompletedProcess:
        self.ended.wait()
        return subprocess.CompletedProcess(
            self.proc.args, self.proc.returncode, self.stdout, self.stderr
        )


class BackgroundRun:
    def __init__(self, container: Container, process: BackgroundProcess):
        self.container = container
        self.process = process

    def signal(self, sig_name: str) -> None:
        self.process.signal(sig_name)

    def pid(self) -> str:
        return self.process.pid()

    def wait(self) -> Run:
        ended = self.process.wait()
        return self.container.recorded(ended.returncode, ended.stdout, ended.stderr)


class Gate:
    """Holds the first request to the mirror whose path matches, until the test releases it."""

    DIRECTORY = "/run/mix-gate"

    def __init__(self, container: Container):
        self.container = container

    def reached(self, unless: BackgroundRun | None = None) -> str:
        """The held path, once a request is held; fails if `unless` ends first."""
        reader = subprocess.Popen(
            ["podman", "exec", self.container.name, "cat", f"{self.DIRECTORY}/reached"],
            stdout=subprocess.PIPE,
            text=True,
        )
        done = threading.Event()
        threading.Thread(target=lambda: (reader.wait(), done.set()), daemon=True).start()
        if unless is not None:
            threading.Thread(
                target=lambda: (unless.process.ended.wait(), done.set()), daemon=True
            ).start()
        done.wait()
        if reader.poll() is None:
            reader.kill()
            ended = unless.wait()
            raise AssertionError(f"the run ended before the gate was reached: {ended!r}")
        return reader.stdout.read().strip()

    def release(self) -> None:
        self.container.exec("sh", "-c", f"echo go > {self.DIRECTORY}/release", check=True)


class Container:
    def __init__(self, name: str):
        self.name = name
        self.runs: list[tuple[list[str], Run]] = []
        self.background: list[str] = []

    def recorded(self, returncode: int, stdout: str, stderr: str, args=()) -> Run:
        run = Run(returncode, stdout, stderr, document_of(stdout) or {})
        self.runs.append((list(args), run))
        if run.document:
            assert run.exit_code == returncode, f"the document says exit {run.exit_code}: {run!r}"
        return run

    def mix(self, *args, env=None, user=None) -> Run:
        result = self.exec("mix", "--json", *args, env=env, user=user)
        return self.recorded(result.returncode, result.stdout, result.stderr, args)

    def gate(self, pattern: str) -> Gate:
        """Puts a gate between this container and the mirror, holding `pattern`'s first request."""
        url = os.environ[mirror.MIRROR_URL_ENV]
        host, port = url.removeprefix("http://").split(":")
        upstream = self.exec("getent", "hosts", host, check=True).stdout.split()[0]
        self.exec("mkdir", "-p", Gate.DIRECTORY, check=True)
        for fifo in ("ready", "reached", "release"):
            self.exec("mkfifo", f"{Gate.DIRECTORY}/{fifo}", check=True)
        subprocess.run(
            ["podman", "cp", str(TESTS_ROOT / "support/gate.py"), f"{self.name}:/run/mix-gate.py"],
            check=True,
        )
        self.exec(
            "sh",
            "-c",
            f"grep -v ' {host}$' /etc/hosts > /tmp/hosts && echo '127.0.0.2 {host}' >> /tmp/hosts"
            " && cat /tmp/hosts > /etc/hosts",
            check=True,
        )
        self.start_background(
            "python3", "/run/mix-gate.py", "127.0.0.2", port, upstream, pattern, Gate.DIRECTORY
        )
        self.exec("cat", f"{Gate.DIRECTORY}/ready", check=True)
        return Gate(self)

    def traced_mix(self, *args, user: str) -> tuple[Run, list[trace.Write]]:
        output = f"/tmp/mix-{len(self.runs)}.strace"
        account = self.exec("getent", "passwd", user, check=True).stdout.split(":")
        setuid_root = self.exec(
            "find",
            "/",
            "-xdev",
            "-type",
            "f",
            "-user",
            "root",
            "-perm",
            "-4000",
            check=True,
        ).stdout.split()
        result = self.exec(
            "strace",
            "-f",
            "-qq",
            "-y",
            "-o",
            output,
            "-e",
            f"trace={trace.SYSCALLS}",
            "-u",
            user,
            "env",
            f"HOME={account[5]}",
            f"USER={user}",
            f"LOGNAME={user}",
            "mix",
            "--json",
            *args,
        )
        run = self.recorded(result.returncode, result.stdout, result.stderr, args)
        calls = self.exec("cat", output, check=True).stdout
        return run, trace.writes(calls, int(account[2]), set(setuid_root))

    @contextlib.contextmanager
    def traced_daemon(self, unit: str = "mix-daemon.service"):
        output = f"/tmp/{unit}.strace"
        writes: list[trace.Write] = []
        self.exec("systemctl", "start", unit, check=True)
        pid = self.exec(
            "systemctl", "show", "--property", "MainPID", "--value", unit, check=True
        ).stdout.strip()
        tracer = self.start_background(
            "strace",
            "-f",
            "-qq",
            "-y",
            "-o",
            output,
            "-e",
            f"trace={trace.SYSCALLS}",
            "-p",
            pid,
        )
        while self._tracer_of(pid) == "0":
            if tracer.ended.is_set():
                raise AssertionError(f"strace ended before it attached to {unit}: {tracer.stderr}")
        try:
            yield writes
        finally:
            self.exec("kill", "-INT", tracer.pid(), check=True)
            tracer.wait()
            calls = self.exec("cat", output, check=True).stdout
            writes.extend(trace.writes(calls, 0, set()))

    def _tracer_of(self, pid: str) -> str:
        status = self.exec("cat", f"/proc/{pid}/status", check=True).stdout
        return next(
            line.split()[1] for line in status.splitlines() if line.startswith("TracerPid:")
        )

    def mix_background(self, *args, env=None, user=None) -> BackgroundRun:
        process = self.start_background("mix", "--json", *args, env=env, user=user)
        return BackgroundRun(self, process)

    def snapshot(self) -> str:
        probes = [
            ("units", ["systemctl", "list-units", "--all", "--no-pager", "nix-*", "mix*"]),
            (
                "daemon journal",
                [
                    "journalctl",
                    "--no-pager",
                    "-u",
                    "nix-daemon.service",
                    "-u",
                    "nix-daemon.socket",
                    "-u",
                    "mix-daemon.service",
                    "-u",
                    "mix-daemon.socket",
                ],
            ),
            ("processes", ["ps", "-eo", "pid,ppid,stat,wchan:24,etime,args", "--forest"]),
            ("gate", ["cat", f"{Gate.DIRECTORY}/log"]),
            (
                "mix journal",
                ["sh", "-c", "ls -la /var/lib/mix/journal && cat /var/lib/mix/journal/*"],
            ),
        ]
        sections = []
        for title, probe in probes:
            result = self.exec(*probe)
            sections.append(f"=== {title}\n{result.stdout}{result.stderr}")
        return "\n".join(sections)

    def attach_failure(self, test: str, report) -> None:
        folder = REPO_ROOT / "target/e2e-events" / re.sub(r"[^\w.-]+", "_", test)
        shutil.rmtree(folder, ignore_errors=True)
        folder.mkdir(parents=True)
        for index, (args, run) in enumerate(self.runs):
            name = f"mix-{index}"
            contents = (
                f"$ mix --json {' '.join(args)}\nexit {run.returncode}\n"
                f"--- stdout\n{run.stdout}\n--- stderr\n{run.stderr}"
            )
            (folder / f"{name}.txt").write_text(contents)
            report.sections.append((name, contents))
        (folder / "machine.txt").write_text(self.snapshot())
        report.user_properties.append(("failure bundle", str(folder)))

    def exec(self, *args, env=None, check=False, user=None):
        cmd = ["podman", "exec"]
        for key, value in (env or {}).items():
            cmd += ["-e", f"{key}={value}"]
        if user:
            cmd += ["-u", user]
        cmd += [self.name, *args]
        result = subprocess.run(cmd, capture_output=True, text=True, check=False)
        if check and result.returncode != 0:
            raise AssertionError(f"{args} failed ({result.returncode}): {result.stderr}")
        return result

    def start_background(self, *args, env=None, user=None) -> BackgroundProcess:
        pidfile = f"/tmp/mix-background-{len(self.background)}.pid"
        self.background.append(pidfile)
        cmd = ["podman", "exec"]
        for key, value in (env or {}).items():
            cmd += ["-e", f"{key}={value}"]
        if user:
            cmd += ["-u", user]
        cmd += [self.name, "sh", "-c", 'echo $$ > "$0" && exec "$@"', pidfile, *args]
        proc = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        return BackgroundProcess(self, proc, pidfile)

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
            "--cap-add=SYS_PTRACE",
            "--pids-limit=-1",
            "--volume",
            f"{binary}:/usr/local/bin/mix:ro",
            "--volume",
            f"{binary.with_name('mix-daemon')}:/usr/local/bin/mix-daemon:ro",
            "--name",
            name,
            image,
        ],
        check=True,
    )
    while True:
        probe = subprocess.run(
            ["podman", "exec", name, "systemctl", "is-system-running", "--wait"],
            capture_output=True,
            text=True,
            check=False,
        )
        if probe.stdout.strip() in ("running", "degraded"):
            return name
        state = subprocess.run(
            ["podman", "inspect", "-f", "{{json .State}}", name],
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip()
        if probe.stdout.strip() or '"Running":true' not in state:
            break
    logs = subprocess.run(["podman", "logs", name], capture_output=True, text=True, check=False)
    subprocess.run(["podman", "rm", "-f", name], capture_output=True, check=False)
    raise RuntimeError(
        f"container systemd ended booting as {probe.stdout.strip()!r}\n"
        f"exec exited {probe.returncode}: {probe.stderr.strip()}\n"
        f"state: {state}\n"
        f"logs: {logs.stdout.strip()}{logs.stderr.strip()}"
    )


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
        ["cargo", "build", "--release", "-p", "mix-cli", "-p", "mix-daemon"],
        cwd=REPO_ROOT,
        check=True,
    )
    return REPO_ROOT / "target/release/mix"


@pytest.fixture(scope="session")
def container_image():
    containerfile = TESTS_ROOT / "Containerfile"
    digest = hashlib.sha256(containerfile.read_bytes()).hexdigest()
    local = f"{IMAGE_REPOSITORY}:{digest}"
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(CACHE_DIR / "container-image.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if REMOTE_IMAGE:
            remote = f"{REMOTE_IMAGE}:{digest}"
            pull = subprocess.run(["podman", "pull", remote], capture_output=True, check=False)
            if pull.returncode == 0:
                return remote
        if subprocess.run(["podman", "image", "exists", local], check=False).returncode != 0:
            subprocess.run(
                [
                    "podman",
                    "build",
                    "-q",
                    "-t",
                    local,
                    "-f",
                    str(containerfile),
                    str(containerfile.parent),
                ],
                check=True,
            )
    return local


def reap_orphans() -> None:
    names = subprocess.run(
        ["podman", "ps", "-a", "--format", "{{.Names}}"],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.split()
    for name in names:
        match = CONTAINER_NAME.fullmatch(name)
        if match and not resources.alive(int(match.group(1))):
            subprocess.run(["podman", "rm", "-f", name], capture_output=True, check=False)
    tags = subprocess.run(
        [
            "podman",
            "images",
            "--filter",
            f"reference={SNAPSHOT_REPOSITORY}",
            "--format",
            "{{.Tag}}",
        ],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.split()
    for tag in tags:
        match = SNAPSHOT_TAG.fullmatch(tag)
        if match and not resources.alive(int(match.group(1))):
            subprocess.run(
                ["podman", "rmi", "-f", f"{SNAPSHOT_REPOSITORY}:{tag}"],
                capture_output=True,
                check=False,
            )


def snapshot_image() -> str:
    controller = os.environ["MIX_TEST_CONTROLLER_PID"]
    return f"{SNAPSHOT_REPOSITORY}:{controller}-{os.environ['MIX_TEST_SESSION']}"


def remove_snapshot() -> None:
    subprocess.run(["podman", "rmi", "-f", snapshot_image()], capture_output=True, check=False)


def _bootstrapped_image(container_image, mix_binary, mirror_server, cache) -> str:
    tag = snapshot_image()
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(SNAPSHOT_LOCK, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if subprocess.run(["podman", "image", "exists", tag], check=False).returncode == 0:
            return tag
        name = f"mix-test-{os.getpid()}-{time.time_ns()}"
        waited = resources.admit(name, SNAPSHOT_TEST, resources.demand_for(SNAPSHOT_TEST, _known()))
        try:
            started = time.monotonic()
            _start_container(container_image, name, mix_binary)
            cgroup = _cgroup_of(name)
            built = Container(name)
            create_user(built, BOOTSTRAPPED_USER, sudo=True)
            mirror.bootstrap_as(built, BOOTSTRAPPED_USER, mirror_server, cache)
            resources.record(SNAPSHOT_TEST, cgroup, time.monotonic() - started, waited, "build")
            subprocess.run(["podman", "stop", name], check=True, capture_output=True)
            subprocess.run(
                ["podman", "commit", "--quiet", name, tag],
                check=True,
                capture_output=True,
            )
        finally:
            subprocess.run(["podman", "rm", "-f", name], capture_output=True, check=False)
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


@pytest.fixture
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
        container = Container(name)
        yield container
    finally:
        if cgroup is not None:
            variant = "snapshot" if bootstrapped else "fresh"
            resources.record(test, cgroup, time.monotonic() - started, waited, variant)
        subprocess.run(["podman", "rm", "-f", name], capture_output=True, check=False)
        resources.release(name)
