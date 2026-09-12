import os
import pathlib
import re
import subprocess
import time

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
CONTAINERFILE = REPO_ROOT / "tests/bootstrap/Containerfile"
IMAGE_TAG = "mix-live-smoke:latest"


def _pinned_nix_url(target: str) -> str:
    pins_src = (REPO_ROOT / "crates/bootstrap/src/pins.rs").read_text()
    block_match = re.search(
        rf'target:\s*"{re.escape(target)}"\s*,(.*?)\n\s*\}},',
        pins_src,
        re.S,
    )
    if not block_match:
        raise RuntimeError(f"no pin found for {target} in pins.rs")
    url_match = re.search(r'url:\s*"([^"]+)"', block_match.group(1))
    if not url_match:
        raise RuntimeError(f"could not parse url for {target} in pins.rs")
    return url_match.group(1)


@pytest.fixture(scope="session")
def nix_mirror_base():
    return _pinned_nix_url("x86_64-linux").rsplit("/", 1)[0]


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
    subprocess.run(
        [
            "podman",
            "build",
            "-q",
            "-t",
            IMAGE_TAG,
            "-f",
            str(CONTAINERFILE),
            str(CONTAINERFILE.parent),
        ],
        check=True,
    )
    return IMAGE_TAG


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
            raise AssertionError(
                f"{args} failed ({result.returncode}): {result.stderr or result.stdout}"
            )
        return result

    def nix_exec(self, *nix_args, check=False):
        command = ". /etc/profile.d/mix-nix.sh && " + " ".join(nix_args)
        return self.exec("bash", "-lc", command, check=check)


def _start_container(image: str) -> str:
    name = f"mix-smoke-{os.getpid()}-{time.time_ns()}"
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


@pytest.fixture(scope="module")
def bootstrapped_container(container_image, mix_binary):
    name = _start_container(container_image)
    container = Container(name)
    try:
        subprocess.run(["podman", "cp", str(mix_binary), f"{name}:/usr/local/bin/mix"], check=True)
        result = container.exec("mix", "-vv", "bootstrap")
        assert result.returncode == 0, (
            f"mix bootstrap failed against the real internet:\n{result.stdout}\n{result.stderr}"
        )
        yield container
    finally:
        subprocess.run(["podman", "rm", "-f", name], capture_output=True)


@pytest.fixture(scope="module")
def fresh_container(container_image, mix_binary):
    name = _start_container(container_image)
    container = Container(name)
    try:
        subprocess.run(["podman", "cp", str(mix_binary), f"{name}:/usr/local/bin/mix"], check=True)
        yield container
    finally:
        subprocess.run(["podman", "rm", "-f", name], capture_output=True)
