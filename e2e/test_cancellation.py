import time

import pytest
from support.container import create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, mirror_args

USER = MIRROR_TEST_USERS[0]
MIX = "/usr/local/bin/mix"
STATE_DIR = f"/home/{USER}/.local/state/mix"
NIX_BUILD = "bin/nix build .*--print-out-paths"
JOURNAL_DIR = "/var/lib/mix/journal"


def _pid_of(container, pattern: str, timeout: float = 60.0) -> str:
    deadline = time.time() + timeout
    while time.time() < deadline:
        pids = container.exec("pgrep", "-f", pattern).stdout.split()
        if pids:
            return pids[0]
        time.sleep(0.05)
    raise TimeoutError(f"no process matching {pattern!r} appeared")


def _pgid(container, pid: str) -> str:
    return container.exec("ps", "-o", "pgid=", "-p", pid, check=True).stdout.strip()


def _state(container, pid: str) -> str:
    return container.exec("ps", "-o", "stat=", "-p", pid).stdout.strip()


def _gone(container, pid: str) -> bool:
    return container.exec("kill", "-0", pid).returncode != 0


def _wait_for_state(container, pid: str, wanted, timeout: float = 10.0) -> str:
    deadline = time.time() + timeout
    while time.time() < deadline:
        state = _state(container, pid)
        if wanted(state):
            return state
        time.sleep(0.05)
    raise TimeoutError(f"process {pid} never reached the wanted state, last {state!r}")


@pytest.mark.bootstrapped
def test_an_interrupted_install_stops_a_frozen_nix_then_puts_the_list_back(
    container, mock_nix_server, mirror_cache
):
    state_before = container.exec("cat", f"{STATE_DIR}/state", check=True).stdout
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    build = _pid_of(container, NIX_BUILD)
    container.exec("kill", "-STOP", build, check=True)
    client = install.pid()
    assert _pgid(container, build) != _pgid(container, client)

    container.exec("kill", "-INT", client, check=True)
    run = install.wait(timeout=30)

    assert run.status == "STATUS_CANCELLED", run
    assert run.cancellation == "CANCELLATION_INTERRUPTED", run
    assert run.progress("stopping"), run
    assert _gone(container, build)
    assert (
        container.exec("cat", f"{STATE_DIR}/state", check=True).stdout == state_before
    )
    home_nix = container.exec("cat", f"{STATE_DIR}/home.nix", check=True).stdout
    assert INSTALL_TEST_PACKAGE not in home_nix


def _wait_until_gone(container, pid: str, timeout: float = 60.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if _gone(container, pid):
            return
        time.sleep(0.05)
    raise TimeoutError(f"process {pid} is still running:\n{container.process_tree()}")


def test_a_second_ctrl_c_leaves_the_worker_to_finish_the_rollback(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    mirror = mirror_args(mock_nix_server, mirror_cache)
    bootstrap = container.mix_background("bootstrap", *mirror, user=USER)
    build = _pid_of(container, NIX_BUILD, timeout=180)
    container.exec("kill", "-STOP", build, check=True)
    worker = _pid_of(container, f"^{MIX} worker")
    client = bootstrap.pid()
    assert _pgid(container, build) != _pgid(container, worker)

    container.exec("kill", "-INT", client, check=True)
    bootstrap.wait_for_progress("stopping", timeout=15)
    container.exec("kill", "-INT", client, check=True)
    run = bootstrap.wait(timeout=30, complete=False)

    assert run.returncode == 130, run
    assert not [
        e for e in run.envelopes if e.get("nodeFinished", {}).get("id") == "1"
    ], "a client that detached cannot have seen the end, and must not invent one"
    _wait_until_gone(container, worker)
    assert _gone(container, build)
    journals = container.exec("ls", "-A", JOURNAL_DIR).stdout
    assert journals == "", f"the worker left an unfinished rollback: {journals}"

    again = container.mix("bootstrap", *mirror, user=USER)
    assert again.succeeded(), again
    assert not [step for step in again.steps() if step == "recover"], again


def test_ctrl_z_pauses_the_workers_nix_and_resuming_lets_bootstrap_finish(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    bootstrap = container.mix_background(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    build = _pid_of(container, NIX_BUILD, timeout=180)
    client = bootstrap.pid()

    container.exec("kill", "-TSTP", client, check=True)

    _wait_for_state(container, build, lambda state: state.startswith("T"))
    _wait_for_state(container, client, lambda state: state.startswith("T"))
    container.exec("kill", "-CONT", client, check=True)
    _wait_for_state(container, build, lambda state: not state.startswith("T"))
    run = bootstrap.wait(timeout=300)

    assert run.succeeded(), run


@pytest.mark.bootstrapped
def test_ctrl_z_pauses_nix_and_resuming_lets_the_install_finish(
    container, mock_nix_server, mirror_cache
):
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    build = _pid_of(container, NIX_BUILD)
    client = install.pid()

    container.exec("kill", "-TSTP", client, check=True)

    _wait_for_state(container, client, lambda state: state.startswith("T"))
    _wait_for_state(container, build, lambda state: state.startswith("T"))
    container.exec("kill", "-CONT", client, check=True)
    _wait_for_state(container, build, lambda state: not state.startswith("T"))
    run = install.wait(timeout=120)

    assert run.succeeded(), run
    assert run.result("install")["added"] == [INSTALL_TEST_PACKAGE]
    assert (
        INSTALL_TEST_PACKAGE
        in container.exec("cat", f"{STATE_DIR}/state", check=True).stdout
    )
