import re
import subprocess

import pytest

from support.container import REPO_ROOT, nix_build
from support.mirror import MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]

BINARY = "/usr/local/bin/mix-conformance"
SEEDS = "/var/tmp/mix-conformance-seeds"
REPO_SEEDS = REPO_ROOT / "crates" / "conformance" / "seeds"
PROFILE_GIT = f"/home/{USER}/.nix-profile/bin/git"


@pytest.fixture(scope="session")
def conformance_binary():
    return nix_build("checks.{system}.conformance") / "bin/mix-conformance"


@pytest.mark.parametrize(
    "suite",
    ["files", "accounts", "units", pytest.param("git", marks=pytest.mark.bootstrapped)],
)
def test_the_machine_does_what_the_model_says(container, conformance_binary, suite):
    REPO_SEEDS.mkdir(parents=True, exist_ok=True)
    subprocess.run(["podman", "cp", conformance_binary, f"{container.name}:{BINARY}"], check=True)
    subprocess.run(["podman", "cp", f"{REPO_SEEDS}/.", f"{container.name}:{SEEDS}"], check=True)
    pinned = []
    if suite == "git":
        git = container.exec("readlink", "-f", PROFILE_GIT, check=True).stdout.strip()
        pinned = ["--git", git]

    result = container.exec(BINARY, suite, "--seeds", SEEDS, *pinned)

    subprocess.run(
        ["podman", "cp", f"{container.name}:{SEEDS}/.", str(REPO_SEEDS)],
        check=True,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert f"{suite}: 256 cases passed" in result.stdout, result.stdout


@pytest.mark.bootstrapped
def test_the_versions_mix_evaluates_at_a_profiles_pins_are_the_ones_it_lists(
    container, conformance_binary, mock_nix_server, mirror_cache
):
    subprocess.run(["podman", "cp", conformance_binary, f"{container.name}:{BINARY}"], check=True)

    result = container.exec(BINARY, "nix", "--user", USER, "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stdout + result.stderr
    assert re.search(r"^nix: [1-9][0-9]* cases passed$", result.stdout, re.MULTILINE), result.stdout


@pytest.mark.bootstrapped
def test_every_profile_action_and_its_undo_do_what_the_model_says(
    container, conformance_binary, mock_nix_server, mirror_cache
):
    subprocess.run(["podman", "cp", conformance_binary, f"{container.name}:{BINARY}"], check=True)

    result = container.exec(BINARY, "profiles", "--user", USER)

    assert result.returncode == 0, result.stdout + result.stderr
    assert re.search(r"^profiles: [1-9][0-9]* cases passed$", result.stdout, re.MULTILINE), (
        result.stdout
    )


def test_installing_and_removing_the_runtime_do_what_the_model_says(
    container, conformance_binary, mock_nix_server
):
    subprocess.run(["podman", "cp", conformance_binary, f"{container.name}:{BINARY}"], check=True)

    result = container.exec(BINARY, "runtime", "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stdout + result.stderr
    assert re.search(r"^runtime: [1-9][0-9]* cases passed$", result.stdout, re.MULTILINE), (
        result.stdout
    )
