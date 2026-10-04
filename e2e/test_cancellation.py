import pytest

from support.container import create_user
from support.mirror import (
    INSTALL_TEST_PACKAGE,
    MIRROR_TEST_USERS,
    bootstrap_activation,
    install_activation,
    mirror_args,
)

USER = MIRROR_TEST_USERS[0]
DAEMON = "/usr/local/bin/mix-daemon"
STATE_DIR = f"/home/{USER}/.local/state/mix"
NIX_BUILD = "bin/nix build .*--print-out-paths"
JOURNAL_DIR = "/var/lib/mix/journal"


def _pid_of(container, pattern: str) -> str:
    return container.exec("pgrep", "-f", pattern, check=True).stdout.split()[0]


def _pgid(container, pid: str) -> str:
    return container.exec("ps", "-o", "pgid=", "-p", pid, check=True).stdout.strip()


def _state(container, pid: str) -> str:
    return container.exec("ps", "-o", "stat=", "-p", pid).stdout.strip()


def _gone(container, pid: str) -> bool:
    return container.exec("kill", "-0", pid).returncode != 0


def _until_state(container, pid: str, wanted) -> None:
    """Returns once `pid` is in a state `wanted` accepts; fails if it exits first."""
    while not wanted(state := _state(container, pid)):
        if not state:
            raise AssertionError(f"process {pid} exited:\n{container.process_tree()}")


def _until_gone(container, pid: str) -> None:
    container.exec("tail", f"--pid={pid}", "-f", "/dev/null", check=True)


@pytest.mark.bootstrapped
def test_an_interrupted_install_stops_a_held_nix_then_puts_the_list_back(
    container, mock_nix_server, mirror_cache
):
    state_before = container.exec("cat", f"{STATE_DIR}/state", check=True).stdout
    gate = container.gate(install_activation(mirror_cache))
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=install)
    build = _pid_of(container, NIX_BUILD)
    client = install.pid()
    assert _pgid(container, build) != _pgid(container, client)

    container.exec("kill", "-INT", client, check=True)
    run = install.wait()

    assert run.status == "STATUS_CANCELLED", run
    assert run.cancellation == "CANCELLATION_INTERRUPTED", run
    assert _gone(container, build)
    assert container.exec("cat", f"{STATE_DIR}/state", check=True).stdout == state_before
    home_nix = container.exec("cat", f"{STATE_DIR}/home.nix", check=True).stdout
    assert INSTALL_TEST_PACKAGE not in home_nix


def test_a_second_ctrl_c_leaves_the_worker_to_finish_the_rollback(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    mirror = mirror_args(mock_nix_server, mirror_cache)
    gate = container.gate(bootstrap_activation(mirror_cache))
    bootstrap = container.mix_background("bootstrap", *mirror, user=USER)
    gate.reached(unless=bootstrap)
    build = _pid_of(container, NIX_BUILD)
    worker = _pid_of(container, f"^{DAEMON} serve-stdin")
    client = bootstrap.pid()
    assert _pgid(container, build) != _pgid(container, worker)

    container.exec("kill", "-INT", client, check=True)
    _until_gone(container, build)
    container.exec("kill", "-INT", client, check=True)
    run = bootstrap.wait()

    assert run.returncode == 130, run
    assert run.document == {}, "a client that detached cannot have seen the end"
    _until_gone(container, worker)
    journals = container.exec("ls", "-A", JOURNAL_DIR).stdout
    assert journals == "", f"the worker left an unfinished rollback: {journals}"

    again = container.mix("bootstrap", *mirror, user=USER)
    assert again.succeeded(), again
    assert "recover" not in again.steps(), again


def test_ctrl_z_pauses_the_workers_nix_and_resuming_lets_bootstrap_finish(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    gate = container.gate(bootstrap_activation(mirror_cache))
    bootstrap = container.mix_background(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    gate.reached(unless=bootstrap)
    build = _pid_of(container, NIX_BUILD)
    client = bootstrap.pid()

    container.exec("kill", "-TSTP", client, check=True)

    _until_state(container, build, lambda state: state.startswith("T"))
    _until_state(container, client, lambda state: state.startswith("T"))
    container.exec("kill", "-CONT", client, check=True)
    _until_state(container, build, lambda state: not state.startswith("T"))
    gate.release()
    run = bootstrap.wait()

    assert run.succeeded(), run


@pytest.mark.bootstrapped
def test_ctrl_z_pauses_nix_and_resuming_lets_the_install_finish(
    container, mock_nix_server, mirror_cache
):
    gate = container.gate(install_activation(mirror_cache))
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=install)
    build = _pid_of(container, NIX_BUILD)
    client = install.pid()

    container.exec("kill", "-TSTP", client, check=True)

    _until_state(container, client, lambda state: state.startswith("T"))
    _until_state(container, build, lambda state: state.startswith("T"))
    container.exec("kill", "-CONT", client, check=True)
    _until_state(container, build, lambda state: not state.startswith("T"))
    gate.release()
    run = install.wait()

    assert run.succeeded(), run
    assert run.result("install")["added"] == [INSTALL_TEST_PACKAGE]
    assert INSTALL_TEST_PACKAGE in container.exec("cat", f"{STATE_DIR}/state", check=True).stdout
