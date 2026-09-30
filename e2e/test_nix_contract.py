import json
import os

import pytest
from support.container import NIX_BINARY
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, bootstrap_root
from support.paths import REPO_ROOT

USER = MIRROR_TEST_USERS[0]

FIXTURES = REPO_ROOT / "crates/core/fixtures/nix"
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


def _fixture(name: str, observed: str) -> str:
    path = FIXTURES / name
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
        record
        for record in records
        if record["action"] == "start" and record["type"] == ACT_BUILD
    ]


def test_a_build_starting_is_logged_the_way_mix_reads_it(container, mock_nix_server):
    _with_plan(container, mock_nix_server)
    drv_path = _nix(
        container, "eval", "--raw", "-f", PLAN, "failing.drvPath"
    ).stdout.strip()

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

    (expected,) = _build_starts(_fixture("build-log.txt", result.stderr))
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
    (expected,) = _error_messages(_fixture("missing-log.txt", result.stderr))
    assert message == expected


@pytest.mark.bootstrapped
def test_nix_never_rewrites_the_lock(container, mock_nix_server, mirror_cache):
    lock = f"/home/{USER}/.local/state/mix/flake.lock"
    rendered = (REPO_ROOT / "crates/nixgen/tests/fixtures/flake.lock").read_text()
    assert container.exec("cat", lock, check=True).stdout == rendered

    run = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)

    assert run.succeeded(), run
    assert container.exec("cat", lock, check=True).stdout == rendered
