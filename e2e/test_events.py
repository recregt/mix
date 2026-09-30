import json

import pytest

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
    assert int(root.get("exitCode", 0)) == result.returncode, result.stdout + result.stderr
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
