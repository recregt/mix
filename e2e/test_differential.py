import json
import subprocess

import pytest

from support.container import REPO_ROOT

BINARY = "/usr/local/bin/mix-differential"


@pytest.fixture(scope="session")
def differential_binary():
    build = subprocess.run(
        [
            "cargo",
            "test",
            "--release",
            "-p",
            "mix-shell",
            "--test",
            "differential",
            "--no-run",
            "--message-format=json",
        ],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            if message["target"]["name"] == "differential":
                return message["executable"]
    raise RuntimeError("cargo built no differential test binary")


def test_root_only_actions_do_what_the_model_predicts(container, differential_binary):
    subprocess.run(
        ["podman", "cp", differential_binary, f"{container.name}:{BINARY}"],
        check=True,
    )

    result = container.exec(BINARY, "--ignored", "--test-threads=1")

    assert result.returncode == 0, result.stdout + result.stderr
    assert "3 passed" in result.stdout, result.stdout
