import json
import time

import pytest
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]
SERVICE = "mix-daemon.service"
UNITS = ("/etc/systemd/system/mix-daemon.socket", "/etc/systemd/system/mix-daemon.service")
STATE_DIR = f"/home/{USER}/.local/state/mix"
STATE = f"{STATE_DIR}/state"
GENERATION_STATE = f"/home/{USER}/.local/state/nix/profiles/home-manager/mix-state"
PACKAGE_BIN = f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"
NIX_BUILD = "bin/nix build .*--print-out-paths"
HOME_MANAGER = f"/home/{USER}/.local/state/nix/profiles/home-manager"


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


def _frozen(container, pattern: str, what: str) -> str:
    def process():
        pids = container.exec("pgrep", "-u", USER, "-f", pattern).stdout.split()
        return pids[0] if pids else None

    pid = _until(process, what, timeout=180)
    container.exec("kill", "-STOP", pid, check=True)
    return pid


def _frozen_build(container) -> str:
    return _frozen(container, NIX_BUILD, f"a nix build for {USER}")


def _killed(container) -> None:
    before = _show(container, "MainPID")
    container.exec(
        "systemctl", "kill", "--kill-whom=main", "--signal=SIGKILL", SERVICE, check=True
    )
    _until(
        lambda: _show(container, "MainPID") not in ("0", before)
        and _show(container, "ActiveState") == "active",
        "the daemon coming back",
    )


def _cat(container, path: str) -> str:
    return container.exec("cat", path, check=True).stdout


def _assert_consistent(container) -> None:
    state = _cat(container, STATE)
    assert state == _cat(container, GENERATION_STATE)
    listed = INSTALL_TEST_PACKAGE in json.loads(state)["packages"]
    assert listed == container.path_exists(PACKAGE_BIN)


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


@pytest.mark.bootstrapped
def test_a_daemon_killed_before_the_switch_is_rolled_back_when_it_comes_back(
    container, mock_nix_server, mirror_cache
):
    state_before = _cat(container, STATE)
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    _frozen_build(container)

    _killed(container)
    run = install.wait(timeout=120, complete=False)

    assert run.returncode != 0, run
    assert container.mix("doctor", user=USER).exit_code == 0
    assert _cat(container, STATE) == state_before
    _assert_consistent(container)
    again = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert again.succeeded(), again
    _assert_consistent(container)


@pytest.mark.bootstrapped
def test_a_daemon_killed_after_the_switch_leaves_a_consistent_profile(
    container, mock_nix_server, mirror_cache
):
    generation = container.exec("readlink", "-f", HOME_MANAGER, check=True).stdout.strip()
    managed = container.exec(
        "bash", "-c",
        f"cd {generation}/home-files && find . -mindepth 1 \\( -type l -o -type f \\) | head -1",
        check=True,
    ).stdout.strip()
    gate = f"/home/{USER}/{managed.removeprefix('./')}"
    container.exec("bash", "-c", f"rm -f {gate} && mkfifo {gate}", user=USER, check=True)
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    _until(
        lambda: container.exec("pgrep", "-u", USER, "-x", "cmp").returncode == 0,
        "activation comparing the gate after the switch",
        timeout=180,
    )

    _killed(container)
    source = f"{generation}/home-files/{managed.removeprefix('./')}"
    container.start_background(
        "bash", "-c", f"while [ -p {gate} ]; do cat {source} > {gate}; done", user=USER
    )
    run = install.wait(timeout=120, complete=False)

    assert run.returncode != 0, run
    gated = container.mix("doctor", user=USER)
    assert gated.exit_code == 3, gated
    assert [
        report["finding"]
        for report in gated.result("doctor")["reports"]
        if "finding" in report
    ] == [{"inTheWay": {"paths": [gate]}}]
    container.exec("rm", "-f", gate, check=True)
    container.exec("pkill", "-u", USER, "-x", "cat")
    assert container.mix("doctor", user=USER).exit_code == 0
    _assert_consistent(container)
    again = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert again.succeeded(), again
    _assert_consistent(container)
