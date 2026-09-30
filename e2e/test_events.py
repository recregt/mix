import json

import pytest

from support.container import root_result
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]
EVENTS = "/tmp/events.ndjson"


def _recorded(container, *command, user=USER):
    result = container.exec("mix", "--events-file", EVENTS, *command, user=user)
    check = container.exec("mix", "events", "check", EVENTS, user=user)
    assert check.returncode == 0, check.stdout + check.stderr
    lines = container.exec("cat", EVENTS, check=True, user=user).stdout.splitlines()
    header = json.loads(lines[0])["header"]
    assert header["format"] == "mix.capture.v1"
    (root,) = [
        record["record"]["envelope"]["nodeFinished"]
        for record in map(json.loads, lines[1:])
        if record["record"]["envelope"].get("nodeFinished", {}).get("id") == "1"
    ]
    assert int(root.get("exitCode", 0)) == result.returncode, (
        result.stdout + result.stderr
    )
    return result, root


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_every_command_records_a_valid_stream_that_carries_its_exit_code(
    container, mock_nix_server, mirror_cache
):
    _, installed = _recorded(container, "install", INSTALL_TEST_PACKAGE)
    assert installed["install"]["added"] == [INSTALL_TEST_PACKAGE]

    _, removed = _recorded(container, "remove", INSTALL_TEST_PACKAGE)
    assert removed["remove"]["removed"] == [INSTALL_TEST_PACKAGE]

    _, healthy = _recorded(container, "doctor")
    assert "doctor" in healthy

    container.exec("rm", "/etc/profile.d/mix-nix.sh", check=True)
    drifted, audited = _recorded(container, "doctor")
    assert drifted.returncode == 3

    _, repaired = _recorded(container, "repair")
    assert any(report.get("fixed") for report in repaired["repair"]["reports"])


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_a_failure_before_any_work_still_streams_its_diagnostic(
    container, mock_nix_server, mirror_cache
):
    result = container.exec("mix", "--output", "json", "install", INSTALL_TEST_PACKAGE)

    assert result.returncode == 1
    assert result.stderr == ""
    envelopes = [json.loads(line) for line in result.stdout.splitlines()]
    (finished,) = [e["nodeFinished"] for e in envelopes if "nodeFinished" in e]
    assert finished["diagnostic"]["code"] == "CODE_ROOT_NOT_ALLOWED"


def _progress(lines, kind):
    envelopes = [json.loads(line) for line in lines if line.strip()]
    envelopes = [
        envelope.get("record", {}).get("envelope", envelope) for envelope in envelopes
    ]
    return [
        envelope["nodeProgress"][kind]
        for envelope in envelopes
        if kind in envelope.get("nodeProgress", {})
    ]


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_the_commands_an_install_runs_are_typed_events_and_stderr_stays_empty(
    container, mock_nix_server, mirror_cache
):
    result = container.exec(
        "mix",
        "--output",
        "json",
        "-vv",
        "--events-file",
        EVENTS,
        "install",
        INSTALL_TEST_PACKAGE,
        user=USER,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stderr == ""
    printed = _progress(result.stdout.splitlines(), "command")
    assert any(
        command["line"].startswith("/") and " build " in command["line"]
        for command in printed
    ), printed

    check = container.exec("mix", "events", "check", EVENTS, user=USER)
    assert check.returncode == 0, check.stdout + check.stderr
    recorded = _progress(
        container.exec("cat", EVENTS, check=True, user=USER).stdout.splitlines()[1:],
        "command",
    )
    assert recorded == printed


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_a_command_run_through_the_worker_streams_json_and_nothing_else(
    container, mock_nix_server, mirror_cache
):
    container.exec("rm", "/etc/profile.d/mix-nix.sh", check=True)

    result = container.exec("mix", "--output", "json", "-vv", "repair", user=USER)

    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stderr == ""
    assert any(
        report.get("fixed")
        for report in root_result(result.stdout)["repair"]["reports"]
    )
