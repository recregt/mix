import json
import time

import pytest
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]
SERVICE = "mix-daemon.service"
UNITS = ("/etc/systemd/system/mix-daemon.socket", "/etc/systemd/system/mix-daemon.service")
STATE_DIR = f"/home/{USER}/.local/state/mix"
NIX_BUILD = "bin/nix build .*--print-out-paths"


def _show(container, prop: str) -> str:
    return container.exec("systemctl", "show", "-P", prop, SERVICE, check=True).stdout.strip()


def _until(wanted, what: str, timeout: float = 60.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        found = wanted()
        if found:
            return found
        time.sleep(0.05)
    raise TimeoutError(f"{what} never happened")


def _frozen_build(container) -> str:
    def build():
        pids = container.exec("pgrep", "-u", USER, "-f", NIX_BUILD).stdout.split()
        return pids[0] if pids else None

    pid = _until(build, f"a nix build for {USER}", timeout=180)
    container.exec("kill", "-STOP", pid, check=True)
    return pid


@pytest.mark.bootstrapped
def test_the_daemon_is_an_enabled_running_service_whose_units_verify(container):
    verify = container.exec("systemd-analyze", "verify", *UNITS)

    assert verify.returncode == 0, verify.stderr
    assert _show(container, "UnitFileState") == "enabled"
    _until(
        lambda: _show(container, "StatusText") == "Waiting for requests",
        "the daemon reporting it waits for requests",
    )
    assert _show(container, "ActiveState") == "active"


@pytest.mark.bootstrapped
def test_stopping_the_daemon_mid_install_rolls_the_install_back(
    container, mock_nix_server, mirror_cache
):
    state_before = container.exec("cat", f"{STATE_DIR}/state", check=True).stdout
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    _frozen_build(container)

    container.exec("systemctl", "stop", "--no-block", SERVICE, check=True)
    run = install.wait(timeout=120)

    assert run.status == "STATUS_CANCELLED", run
    assert run.cancellation == "CANCELLATION_TERMINATED", run
    assert container.exec("cat", f"{STATE_DIR}/state", check=True).stdout == state_before
    _until(lambda: _show(container, "ActiveState") == "inactive", "the daemon stopping")
    assert _show(container, "Result") == "success"
    assert container.mix("doctor", user=USER).exit_code in (0, 3)


@pytest.mark.bootstrapped
def test_a_drain_lets_a_running_install_finish_and_the_daemon_comes_back(
    container, mock_nix_server, mirror_cache
):
    before = _show(container, "MainPID")
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    build = _frozen_build(container)

    container.exec(
        "systemctl", "kill", "--kill-whom=main", "--signal=SIGHUP", SERVICE, check=True
    )
    _until(
        lambda: _show(container, "StatusText").startswith("Finishing"),
        "the daemon reporting its drain",
    )
    container.exec("kill", "-CONT", build, check=True)
    run = install.wait(timeout=300)

    assert run.succeeded(), run
    assert run.result("install")["added"] == [INSTALL_TEST_PACKAGE], run
    _until(
        lambda: _show(container, "MainPID") not in ("0", before)
        and _show(container, "ActiveState") == "active",
        "the daemon coming back",
    )
    assert _show(container, "NRestarts") == "1"


@pytest.mark.bootstrapped
def test_every_request_leaves_an_audit_entry_in_the_journal(container):
    run = container.mix("doctor", user=USER)
    request = run.envelopes[0]["request"]

    def entry():
        lines = container.exec(
            "journalctl", "-o", "json", f"MIX_REQUEST={request}"
        ).stdout.splitlines()
        return json.loads(lines[0]) if lines else None

    found = _until(entry, f"an audit entry for request {request}")

    assert found["MIX_USER"] == USER
    assert found["MIX_COMMAND"] == "doctor"
    assert found["_SYSTEMD_UNIT"] == SERVICE
