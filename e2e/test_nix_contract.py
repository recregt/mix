import json
import os
import re

import pytest

from support.container import NIX_BINARY
from support.mirror import (
    ALLOW_NETWORK_ENV,
    HOME_MANAGER_REV,
    INSTALL_TEST_PACKAGE,
    MIRROR_TEST_USERS,
    NIXPKGS_REV,
    bootstrap_root,
)
from support.paths import REPO_ROOT

USER = MIRROR_TEST_USERS[0]

FIXTURES = REPO_ROOT / "crates/nixlog/fixtures/nix"
PLAN = "/tmp/plan.nix"
ACT_BUILD = 105
UPDATE = os.environ.get("MIX_UPDATE_NIX_FIXTURES", "").strip().lower() in (
    "1",
    "true",
    "yes",
)


def _with_plan(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)
    source = (FIXTURES / "plan.nix").read_text()
    container.exec("bash", "-c", f"cat > {PLAN} <<'EOF'\n{source}EOF", check=True)


def _nix(container, *args):
    return container.exec(NIX_BINARY, *args)


def _fixture(path, observed: str) -> str:
    if UPDATE:
        path.write_text(observed)
    return path.read_text()


def _build_starts(output: str) -> list[dict]:
    records = [
        json.loads(line.removeprefix("@nix "))
        for line in output.splitlines()
        if line.startswith("@nix ")
    ]
    return [
        record for record in records if record["action"] == "start" and record["type"] == ACT_BUILD
    ]


def test_a_build_starting_is_logged_the_way_mix_reads_it(container, mock_nix_server):
    _with_plan(container, mock_nix_server)
    drv_path = _nix(container, "eval", "--raw", "-f", PLAN, "failing.drvPath").stdout.strip()

    result = _nix(
        container,
        "build",
        "-f",
        PLAN,
        "failing",
        "--no-link",
        "--log-format",
        "internal-json",
    )

    starts = _build_starts(result.stderr)
    assert [record["fields"][0] for record in starts] == [drv_path]

    (expected,) = _build_starts(_fixture(FIXTURES / "build-log.txt", result.stderr))
    assert sorted(starts[0]) == sorted(expected)
    assert [type(field) for field in starts[0]["fields"]] == [
        type(field) for field in expected["fields"]
    ]


def _error_messages(output: str) -> list[str]:
    records = [
        json.loads(line.removeprefix("@nix "))
        for line in output.splitlines()
        if line.startswith("@nix ")
    ]
    return [
        record["raw_msg"]
        for record in records
        if record["action"] == "msg" and record["level"] == 0
    ]


def test_a_missing_package_is_reported_the_way_mix_reads_it(container, mock_nix_server):
    _with_plan(container, mock_nix_server)

    result = _nix(
        container,
        "build",
        "-f",
        PLAN,
        "missing",
        "--no-link",
        "--log-format",
        "internal-json",
    )

    assert result.returncode != 0
    (message,) = _error_messages(result.stderr)
    assert "ripgrep2" in message
    (expected,) = _error_messages(_fixture(FIXTURES / "missing-log.txt", result.stderr))
    assert message == expected


@pytest.mark.bootstrapped
def test_nix_never_rewrites_the_lock(container, mock_nix_server, mirror_cache):
    lock = f"/home/{USER}/.local/state/mix/flake.lock"
    rendered = (REPO_ROOT / "crates/nixgen/tests/fixtures/flake.lock").read_text()
    assert container.exec("cat", lock, check=True).stdout == rendered

    run = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)

    assert run.succeeded(), run
    assert container.exec("cat", lock, check=True).stdout == rendered


PREFETCHED = REPO_ROOT / "crates/shell/fixtures/nix"
BRANCHES = REPO_ROOT / "crates/shell/fixtures/git/ls-remote-branches.txt"
PROFILE_GIT = f"/home/{USER}/.nix-profile/bin/git"
BRANCH_LINE = re.compile(r"[0-9a-f]{40}\trefs/heads/\S+")


def _needs_network(what: str):
    if os.environ.get(ALLOW_NETWORK_ENV, "").strip().lower() not in ("1", "true", "yes"):
        pytest.skip(f"{what} needs the network: set {ALLOW_NETWORK_ENV}=1")


def _without_github_api(container):
    container.exec("sh", "-c", "echo '0.0.0.0 api.github.com' >> /etc/hosts", check=True)


@pytest.mark.bootstrapped
@pytest.mark.parametrize(
    ("reference", "fixture"),
    [
        (f"github:NixOS/nixpkgs/{NIXPKGS_REV}", "prefetch-nixpkgs.json"),
        (f"github:nix-community/home-manager/{HOME_MANAGER_REV}", "prefetch-home-manager.json"),
    ],
)
def test_a_prefetched_input_is_printed_the_way_mix_reads_it(
    container, mock_nix_server, mirror_cache, reference, fixture
):
    _needs_network("prefetching from GitHub")
    _without_github_api(container)

    result = container.exec(
        NIX_BINARY,
        "flake",
        "prefetch",
        "--json",
        "--flake-registry",
        "",
        reference,
        user=USER,
        check=True,
    )

    expected = _fixture(PREFETCHED / fixture, result.stdout.strip() + "\n")
    assert json.loads(result.stdout) == json.loads(expected)


@pytest.mark.bootstrapped
@pytest.mark.parametrize(
    ("remote", "branch"),
    [
        ("https://github.com/NixOS/nixpkgs", "nixos-unstable"),
        ("https://github.com/nix-community/home-manager", "master"),
    ],
)
def test_branch_heads_are_listed_the_way_mix_reads_them(
    container, mock_nix_server, mirror_cache, remote, branch
):
    _needs_network("listing branches on GitHub")
    _without_github_api(container)

    result = container.exec(PROFILE_GIT, "ls-remote", "--branches", remote, user=USER, check=True)

    lines = result.stdout.splitlines()
    assert all(BRANCH_LINE.fullmatch(line) for line in lines), lines
    assert any(line.endswith(f"\trefs/heads/{branch}") for line in lines)
    assert all(BRANCH_LINE.fullmatch(line) for line in BRANCHES.read_text().splitlines())


def _unable_to_download(output: str) -> str:
    rest = output[output.rindex("error: unable to download '") :]
    return rest.split("'")[1]


def test_a_failed_download_is_reported_the_way_mix_reads_it(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)
    reference = f"github:NixOS/nixpkgs/{NIXPKGS_REV}?host=127.0.0.1"

    result = _nix(container, "flake", "prefetch", "--json", "--flake-registry", "", reference)

    assert result.returncode != 0
    expected = _fixture(FIXTURES / "download-failed.txt", result.stderr)
    assert _unable_to_download(result.stderr) == _unable_to_download(expected)
