import contextlib
import json
import os
import pathlib
import platform
import subprocess
import threading

import pytest
from periplu import container as periplu
from periplu import trace
from periplu.container import BackgroundProcess

from support import mirror
from support.events import Run, document_of
from support.paths import REPO_ROOT, TESTS_ROOT

NIX_BINARY = "/nix/var/nix/profiles/default/bin/nix"
MIX_USERS_GROUP = "mix-users"
NIX_CONF_DEST = "/etc/nix/nix.conf"

SNAPSHOT_KEY = "bootstrapped"
BOOTSTRAPPED_USER = mirror.MIRROR_TEST_USERS[0]
RUN_ARGS = ["--cap-add=SYS_ADMIN", "--cap-add=SYS_PTRACE", "--pids-limit=-1"]
REDIRECT_HOST = """
import sys

host, address = sys.argv[1:3]
lines = []
with open("/etc/hosts") as hosts:
    for line in hosts.read().splitlines():
        fields = line.split("#", 1)[0].split()
        if host in fields[1:]:
            others = [name for name in fields[1:] if name != host]
            if not others:
                continue
            line = "\\t".join([fields[0], *others])
        lines.append(line)
lines.append(f"{address}\\t{host}")
with open("/etc/hosts", "w") as hosts:
    hosts.write("\\n".join(lines) + "\\n")
"""


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


class Container(periplu.Container):
    def recorded(self, returncode: int, stdout: str, stderr: str, args=()) -> Run:
        run = Run(returncode, stdout, stderr, document_of(stdout) or {})
        self.record(f"mix --json {' '.join(args)}", returncode, stdout, stderr)
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
        self.exec("python3", "-c", REDIRECT_HOST, host, "127.0.0.2", check=True)
        self.start_background(
            "python3", "/run/mix-gate.py", "127.0.0.2", port, upstream, pattern, Gate.DIRECTORY
        )
        self.exec("cat", f"{Gate.DIRECTORY}/ready", check=True)
        return Gate(self)

    def traced_mix(self, *args, user: str) -> tuple[Run, list[trace.Write]]:
        output = f"/tmp/mix-{len(self.recorded_runs)}.strace"
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
        traced = trace.Traced([], [])
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
            yield traced
        finally:
            self.exec("kill", "-INT", tracer.pid(), check=True)
            tracer.wait()
            calls = self.exec("cat", output, check=True).stdout
            traced.writes.extend(trace.writes(calls, 0, set()))
            traced.accesses.extend(trace.accesses(calls, 0, set()))

    def _tracer_of(self, pid: str) -> str:
        status = self.exec("cat", f"/proc/{pid}/status", check=True).stdout
        return next(
            line.split()[1] for line in status.splitlines() if line.startswith("TracerPid:")
        )

    def mix_background(self, *args, env=None, user=None) -> BackgroundRun:
        process = self.start_background("mix", "--json", *args, env=env, user=user)
        return BackgroundRun(self, process)

    def probes(self) -> list[tuple[str, list[str]]]:
        return [
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
            *super().probes(),
            ("gate", ["cat", f"{Gate.DIRECTORY}/log"]),
            (
                "mix journal",
                ["sh", "-c", "ls -la /var/lib/mix/journal && cat /var/lib/mix/journal/*"],
            ),
        ]


def _run_args(binary: pathlib.Path | None) -> list[str]:
    if binary is None:
        return RUN_ARGS
    return [
        *RUN_ARGS,
        "--volume",
        f"{binary}:/usr/local/bin/mix:ro",
        "--volume",
        f"{binary.with_name('mix-daemon')}:/usr/local/bin/mix-daemon:ro",
    ]


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


def nix_build(attribute: str) -> pathlib.Path:
    """Builds a flake output of the repository and returns its store path."""
    system = f"{platform.machine()}-linux"
    built = subprocess.run(
        [
            "nix",
            "build",
            "--no-link",
            "--print-out-paths",
            f"{REPO_ROOT}#{attribute.format(system=system)}",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return pathlib.Path(built.stdout.strip())


@pytest.fixture(scope="session")
def mix_binary():
    return nix_build("packages.{system}.default") / "bin/mix"


@pytest.fixture(scope="session")
def container_image():
    return periplu.image(TESTS_ROOT / "Containerfile")


def _bootstrapped_image(container_image, mix_binary, mirror_server, cache) -> str:
    def bootstrap(name: str) -> None:
        built = Container(name)
        create_user(built, BOOTSTRAPPED_USER, sudo=True)
        mirror.bootstrap_as(built, BOOTSTRAPPED_USER, mirror_server, cache)

    return periplu.snapshot(
        SNAPSHOT_KEY,
        container_image,
        run_args=_run_args(mix_binary),
        systemd=True,
        build=bootstrap,
    )


@pytest.fixture
def container(request, container_image, mix_binary):
    bootstrapped = request.node.get_closest_marker("bootstrapped") is not None
    unmounted = request.node.get_closest_marker("unmounted") is not None
    image = container_image
    if bootstrapped:
        image = _bootstrapped_image(
            container_image,
            mix_binary,
            request.getfixturevalue("mock_nix_server"),
            request.getfixturevalue("mirror_cache"),
        )
    variant = "snapshot" if bootstrapped else "fresh"
    run_args = _run_args(None if unmounted else mix_binary)
    with periplu.admitted(
        request.node.nodeid, image, run_args=run_args, systemd=True, variant=variant
    ) as name:
        yield Container(name)
