import fcntl
import hashlib
import functools
import json
import os
import pathlib
import queue
import re
import shutil
import subprocess
import threading
import time

import pytest

from support import mirror, resources, trace
from support.events import Run, envelopes_of, progress_of, started_step
from support.paths import CACHE_DIR, REPO_ROOT, TESTS_ROOT

NIX_BINARY = "/nix/var/nix/profiles/default/bin/nix"
ERE_SPECIAL = set("\\.[]{}()*+?^$|")
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
        raise TimeoutError(
            f"process matching {self.pattern!r} never appeared in the container"
        )

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
        return subprocess.CompletedProcess(
            self.proc.args, self.proc.returncode, self.output, ""
        )


class BackgroundRun:
    def __init__(
        self, container: "Container", process: BackgroundProcess, capture: str
    ):
        self.container = container
        self.process = process
        self.capture = capture

    def wait_for(self, found, timeout: float = 60.0) -> dict:
        deadline = time.time() + timeout
        while time.time() < deadline:
            line = self.process._next_line(max(0.01, deadline - time.time()))
            if line is None:
                break
            self.process.output += line
            try:
                envelope = json.loads(line)
            except json.JSONDecodeError:
                continue
            if found(envelope):
                return envelope
        if self.process.proc.poll() is not None:
            raise AssertionError(
                f"mix exited ({self.process.proc.returncode}) before the awaited event:\n"
                f"{self.process.output}"
            )
        raise TimeoutError(
            f"the awaited event never came within {timeout:g}s; processes now:\n"
            f"{self.container.process_tree()}\nstream so far:\n{self.process.output}"
        )

    def wait_for_step(self, key: str, timeout: float = 60.0) -> dict:
        return self.wait_for(lambda envelope: started_step(envelope, key), timeout)

    def wait_for_progress(self, kind: str, timeout: float = 60.0) -> dict:
        return self.wait_for(
            lambda envelope: progress_of(envelope, kind) is not None, timeout
        )

    def pid(self, timeout: float = 10.0) -> str:
        return self.process.pid(timeout)

    def signal(self, sig_name: str, timeout: float = 10.0) -> None:
        self.process.signal(sig_name, timeout)

    def wait(self, timeout: float = 60.0, complete: bool = True) -> Run:
        ended = self.process.wait(timeout=timeout)
        stdout = ended.stdout
        if complete:
            return self.container.recorded(self.capture, ended.returncode, stdout, "")
        capture = self.container.exec("cat", self.capture).stdout
        return Run(ended.returncode, stdout, "", envelopes_of(capture))


class Container:
    def __init__(self, name: str):
        self.name = name
        self.captures: list[str] = []

    def capture(self) -> str:
        path = f"/tmp/mix-events-{len(self.captures)}.ndjson"
        self.captures.append(path)
        return path

    def recorded(self, capture: str, returncode: int, stdout: str, stderr: str) -> Run:
        contents = self.exec("cat", capture).stdout
        check = self.exec("mix", "events", "check", capture)
        assert check.returncode == 0, (
            f"{capture} is not a valid stream: {check.stderr}\n{contents}"
        )
        run = Run(returncode, stdout, stderr, envelopes_of(contents))
        assert run.exit_code == returncode, (
            f"the root says exit {run.exit_code}: {run!r}"
        )
        return run

    def mix(self, *args, env=None, user=None) -> Run:
        capture = self.capture()
        result = self.exec("mix", "--events-file", capture, *args, env=env, user=user)
        return self.recorded(capture, result.returncode, result.stdout, result.stderr)

    def traced_mix(self, *args, user: str) -> tuple[Run, list[trace.Write]]:
        capture = self.capture()
        output = f"{capture}.strace"
        account = self.exec("getent", "passwd", user, check=True).stdout.split(":")
        setuid_root = self.exec(
            "find", "/", "-xdev", "-type", "f", "-user", "root", "-perm", "-4000",
            check=True,
        ).stdout.split()
        result = self.exec(
            "strace", "-f", "-qq", "-y", "-o", output, "-e", f"trace={trace.SYSCALLS}",
            "-u", user,
            "env", f"HOME={account[5]}", f"USER={user}", f"LOGNAME={user}",
            "mix", "--events-file", capture, *args,
        )
        run = self.recorded(capture, result.returncode, result.stdout, result.stderr)
        calls = self.exec("cat", output, check=True).stdout
        return run, trace.writes(calls, int(account[2]), set(setuid_root))

    def mix_background(self, *args, env=None, user=None) -> BackgroundRun:
        capture = self.capture()
        process = self.start_background(
            "mix",
            "--output",
            "json",
            "--events-file",
            capture,
            *args,
            env=env,
            user=user,
        )
        return BackgroundRun(self, process, capture)

    def rendered(self, capture: str, verbosity: str) -> str:
        return self.exec("mix", verbosity, "events", "show", capture).stdout

    def snapshot(self) -> str:
        probes = [
            ("units", ["systemctl", "list-units", "--all", "--no-pager", "nix-*", "mix*"]),
            (
                "daemon journal",
                ["journalctl", "--no-pager", "-u", "nix-daemon.service", "-u", "nix-daemon.socket"],
            ),
            ("processes", ["ps", "-eo", "pid,ppid,stat,wchan:24,etime,args", "--forest"]),
            ("mix journal", ["sh", "-c", "ls -la /var/lib/mix/journal && cat /var/lib/mix/journal/*"]),
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
        for capture in self.captures:
            name = pathlib.PurePosixPath(capture).stem
            (folder / f"{name}.ndjson").write_text(self.exec("cat", capture).stdout)
            (folder / f"{name}.txt").write_text(self.rendered(capture, "-vv"))
            report.sections.append(
                (f"mix -v events show {capture}", self.rendered(capture, "-v"))
            )
        (folder / "machine.txt").write_text(self.snapshot())
        report.user_properties.append(("failure bundle", str(folder)))

    def exec(self, *args, env=None, check=False, user=None):
        cmd = ["podman", "exec"]
        for key, value in (env or {}).items():
            cmd += ["-e", f"{key}={value}"]
        if user:
            cmd += ["-u", user]
        cmd += [self.name, *args]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if check and result.returncode != 0:
            raise AssertionError(
                f"{args} failed ({result.returncode}): {result.stderr}"
            )
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
        return BackgroundProcess(self, proc, pattern=literal(" ".join(args)))

    def process_tree(self) -> str:
        return self.exec(
            "ps", "-eo", "pid,ppid,stat,wchan:24,etime,args", "--forest"
        ).stdout

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
    digest = hashlib.sha256(containerfile.read_bytes()).hexdigest()
    local = f"{IMAGE_REPOSITORY}:{digest}"
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(CACHE_DIR / "container-image.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if REMOTE_IMAGE:
            remote = f"{REMOTE_IMAGE}:{digest}"
            pull = subprocess.run(["podman", "pull", remote], capture_output=True)
            if pull.returncode == 0:
                return remote
        if subprocess.run(["podman", "image", "exists", local]).returncode != 0:
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
        ["podman", "ps", "-a", "--format", "{{.Names}}"], capture_output=True, text=True
    ).stdout.split()
    for name in names:
        match = CONTAINER_NAME.fullmatch(name)
        if match and not resources.alive(int(match.group(1))):
            subprocess.run(["podman", "rm", "-f", name], capture_output=True)
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
    ).stdout.split()
    for tag in tags:
        match = SNAPSHOT_TAG.fullmatch(tag)
        if match and not resources.alive(int(match.group(1))):
            subprocess.run(
                ["podman", "rmi", "-f", f"{SNAPSHOT_REPOSITORY}:{tag}"],
                capture_output=True,
            )


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
            resources.record(
                SNAPSHOT_TEST, cgroup, time.monotonic() - started, waited, "build"
            )
            subprocess.run(["podman", "stop", name], check=True, capture_output=True)
            subprocess.run(
                ["podman", "commit", "--quiet", name, tag],
                check=True,
                capture_output=True,
            )
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
        container = Container(name)
        yield container
    finally:
        if cgroup is not None:
            variant = "snapshot" if bootstrapped else "fresh"
            resources.record(test, cgroup, time.monotonic() - started, waited, variant)
        subprocess.run(["podman", "rm", "-f", name], capture_output=True)
        resources.release(name)
