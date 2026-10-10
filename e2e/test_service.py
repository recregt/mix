import json
import subprocess

import pytest

from support.container import until
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, install_activation

USER = MIRROR_TEST_USERS[0]
SERVICE = "mix-daemon.service"
UNITS = ("/etc/systemd/system/mix-daemon.socket", "/etc/systemd/system/mix-daemon.service")
STATE_DIR = f"/home/{USER}/.local/state/mix"
STATE = f"{STATE_DIR}/state"
GENERATION_STATE = f"/home/{USER}/.local/state/nix/profiles/home-manager/mix-state"
PACKAGE_BIN = f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"
HOME_MANAGER = f"/home/{USER}/.local/state/nix/profiles/home-manager"


def _show(container, prop: str) -> str:
    return container.exec("systemctl", "show", "-P", prop, SERVICE, check=True).stdout.strip()


def _service(container, wanted, what: str) -> None:
    """Returns once the service's state is `wanted`; fails as soon as it failed."""

    def reached():
        if _show(container, "ActiveState") == "failed":
            raise AssertionError(f"{SERVICE} failed before {what}")
        return wanted()

    until(reached, what)


def _killed(container) -> None:
    container.exec("systemctl", "kill", "--kill-whom=main", "--signal=SIGKILL", SERVICE, check=True)


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
    _service(
        container,
        lambda: _show(container, "StatusText") == "Waiting for requests",
        "the daemon reporting it waits for requests",
    )
    assert _show(container, "ActiveState") == "active"


@pytest.mark.bootstrapped
def test_stopping_the_daemon_mid_install_rolls_the_install_back(
    container, mock_nix_server, mirror_cache
):
    state_before = container.exec("cat", f"{STATE_DIR}/state", check=True).stdout
    gate = container.gate(install_activation(mirror_cache))
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=install)

    stopping = container.start_background("systemctl", "stop", SERVICE)
    run = install.wait()
    assert stopping.wait().returncode == 0

    assert run.status == "STATUS_CANCELLED", run
    assert run.cancellation == "CANCELLATION_TERMINATED", run
    assert container.exec("cat", f"{STATE_DIR}/state", check=True).stdout == state_before
    assert _show(container, "ActiveState") == "inactive"
    assert _show(container, "Result") == "success"
    assert container.mix("doctor", user=USER).exit_code in (0, 3)


@pytest.mark.bootstrapped
def test_a_drain_lets_a_running_install_finish_and_the_daemon_comes_back(
    container, mock_nix_server, mirror_cache
):
    before = _show(container, "MainPID")
    gate = container.gate(install_activation(mirror_cache))
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=install)

    container.exec("systemctl", "kill", "--kill-whom=main", "--signal=SIGHUP", SERVICE, check=True)
    until(
        lambda: _show(container, "StatusText").startswith("Finishing"),
        "the daemon reporting its drain",
        unless=install,
    )
    gate.release()
    run = install.wait()

    assert run.succeeded(), run
    assert run.result("install")["added"] == [INSTALL_TEST_PACKAGE], run
    assert container.mix("doctor", user=USER).exit_code == 0
    assert _show(container, "MainPID") not in ("0", before)
    assert _show(container, "NRestarts") == "1"


@pytest.mark.bootstrapped
def test_every_request_leaves_an_audit_entry_in_the_journal(container):
    run = container.mix("doctor", user=USER)

    follow = subprocess.Popen(
        [
            "podman",
            "exec",
            container.name,
            "journalctl",
            "-f",
            "-n",
            "all",
            "-o",
            "json",
            f"MIX_REQUEST={run.request}",
        ],
        stdout=subprocess.PIPE,
        text=True,
    )
    found = json.loads(follow.stdout.readline())
    follow.kill()

    assert found["MIX_USER"] == USER
    assert found["MIX_COMMAND"] == "doctor"
    assert found["_SYSTEMD_UNIT"] == SERVICE


@pytest.mark.bootstrapped
def test_a_daemon_killed_before_the_switch_is_rolled_back_when_it_comes_back(
    container, mock_nix_server, mirror_cache
):
    state_before = _cat(container, STATE)
    gate = container.gate(install_activation(mirror_cache))
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=install)

    _killed(container)
    run = install.wait()
    gate.release()

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
        "bash",
        "-c",
        f"cd {generation}/home-files && find . -mindepth 1 \\( -type l -o -type f \\) | head -1",
        check=True,
    ).stdout.strip()
    gate = f"/home/{USER}/{managed.removeprefix('./')}"
    container.exec("bash", "-c", f"rm -f {gate} && mkfifo {gate}", user=USER, check=True)
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    until(
        lambda: container.exec("pgrep", "-u", USER, "-x", "cmp").returncode == 0,
        "activation comparing the gate after the switch",
        unless=install,
    )

    _killed(container)
    source = f"{generation}/home-files/{managed.removeprefix('./')}"
    container.start_background(
        "bash", "-c", f"while [ -p {gate} ]; do cat {source} > {gate}; done", user=USER
    )
    run = install.wait()

    assert run.returncode != 0, run
    gated = container.mix("doctor", user=USER)
    assert gated.exit_code == 3, gated
    assert [
        report["finding"] for report in gated.result("doctor")["reports"] if "finding" in report
    ] == [{"inTheWay": {"paths": [gate]}}]
    container.exec("rm", "-f", gate, check=True)
    container.exec("pkill", "-u", USER, "-x", "cat")
    assert container.mix("doctor", user=USER).exit_code == 0
    _assert_consistent(container)
    again = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert again.succeeded(), again
    _assert_consistent(container)
