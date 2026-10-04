import json
import subprocess

import pytest

from support.container import REPO_ROOT
from support.mirror import MIRROR_TEST_USERS

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
            "--features",
            "machine-tests",
            "--test",
            "machine",
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
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("executable")
            and message["target"]["name"] == "machine"
        ):
            return message["executable"]
    raise RuntimeError("cargo built no machine test binary")


def _differential(container, binary, *names, mirror=""):
    subprocess.run(["podman", "cp", binary, f"{container.name}:{BINARY}"], check=True)
    return container.exec(
        "env",
        f"MIX_DIFFERENTIAL_USER={MIRROR_TEST_USERS[0]}",
        f"MIX_DIFFERENTIAL_MIRROR={mirror}",
        BINARY,
        "--exact",
        "--test-threads=1",
        *names,
    )


def test_root_only_actions_do_what_the_model_predicts(container, differential_binary):
    names = (
        "every_account_action_and_its_undo_match_the_model",
        "every_unit_action_and_its_undo_match_the_model",
        "changing_an_owner_to_another_user_matches_the_model",
    )

    result = _differential(container, differential_binary, *names)

    assert result.returncode == 0, result.stdout + result.stderr
    assert f"{len(names)} passed" in result.stdout, result.stdout


@pytest.mark.bootstrapped
def test_profile_actions_do_what_the_model_predicts(
    container, differential_binary, mock_nix_server, mirror_cache
):
    result = _differential(
        container, differential_binary, "every_profile_action_and_its_undo_match_the_model"
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert "1 passed" in result.stdout, result.stdout


def test_the_runtime_does_what_the_model_predicts(container, differential_binary, mock_nix_server):
    result = _differential(
        container,
        differential_binary,
        "installing_and_removing_the_runtime_match_the_model",
        mirror=mock_nix_server["url"],
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert "1 passed" in result.stdout, result.stdout
