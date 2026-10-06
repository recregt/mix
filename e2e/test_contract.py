import json
import subprocess

import pytest

from support.container import REPO_ROOT

BINARY = "/usr/local/bin/mix-conformance"
SEEDS = "/var/tmp/mix-conformance-seeds"
REPO_SEEDS = REPO_ROOT / "crates" / "conformance" / "seeds"


@pytest.fixture(scope="session")
def conformance_binary():
    build = subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "-p",
            "mix-conformance",
            "--bin",
            "mix-conformance",
            "--message-format=json",
        ],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("executable")
            and message["target"]["name"] == "mix-conformance"
        ):
            return message["executable"]
    raise RuntimeError("cargo built no mix-conformance binary")


@pytest.mark.parametrize("suite", ["files", "accounts", "units"])
def test_the_machine_does_what_the_model_says(container, conformance_binary, suite):
    REPO_SEEDS.mkdir(parents=True, exist_ok=True)
    subprocess.run(["podman", "cp", conformance_binary, f"{container.name}:{BINARY}"], check=True)
    subprocess.run(["podman", "cp", f"{REPO_SEEDS}/.", f"{container.name}:{SEEDS}"], check=True)

    result = container.exec(BINARY, suite, "--seeds", SEEDS)

    subprocess.run(
        ["podman", "cp", f"{container.name}:{SEEDS}/.", str(REPO_SEEDS)],
        check=True,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert f"{suite}: 256 cases passed" in result.stdout, result.stdout
