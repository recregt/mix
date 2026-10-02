import shlex
import time

import pytest
from support.container import create_user
from support.mirror import (
    INSTALL_TEST_PACKAGE,
    MIRROR_TEST_USERS,
    bootstrap_as,
    mirror_args,
)

USER, SECOND_USER = MIRROR_TEST_USERS
NIX_BUILD = "bin/nix build .*--print-out-paths"
PACKAGE_BIN = f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"


def test_install_works_in_the_same_session_as_bootstrap(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    mirror = shlex.join(mirror_args(mock_nix_server, mirror_cache))
    bootstrap, install = container.capture(), container.capture()

    result = container.exec(
        "bash",
        "-c",
        f"mix --events-file {bootstrap} bootstrap {mirror}"
        f" && mix --events-file {install} install {INSTALL_TEST_PACKAGE}",
        user=USER,
    )

    assert container.recorded(bootstrap, 0, "", result.stderr).succeeded()
    installed = container.recorded(install, result.returncode, "", result.stderr)
    assert installed.succeeded(), installed
    assert installed.result("install")["added"] == [INSTALL_TEST_PACKAGE]
    assert container.path_exists(PACKAGE_BIN)


def _frozen_build(container, user: str) -> str:
    deadline = time.time() + 180
    while time.time() < deadline:
        pids = container.exec("pgrep", "-u", user, "-f", NIX_BUILD).stdout.split()
        if pids:
            container.exec("kill", "-STOP", pids[0], check=True)
            return pids[0]
        time.sleep(0.05)
    raise TimeoutError(f"no nix build for {user} appeared")


def _lock_wait(run) -> dict:
    return run.wait_for(lambda envelope: "lockWait" in envelope.get("nodeStarted", {}))[
        "nodeStarted"
    ]["lockWait"]


@pytest.mark.bootstrapped
def test_the_same_users_next_command_waits_and_names_the_one_it_waits_for(
    container, mock_nix_server, mirror_cache
):
    first = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    build = _frozen_build(container, USER)

    second = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    wait = _lock_wait(second)
    assert wait == {"lock": f"user {USER}", "holder": USER, "command": "install"}, wait

    third = container.mix_background("doctor", user=USER)
    _lock_wait(third)
    third.signal("INT")
    stopped = third.wait(timeout=30)
    assert stopped.status == "STATUS_CANCELLED", stopped

    container.exec("kill", "-CONT", build, check=True)
    installed = first.wait(timeout=300)
    assert installed.succeeded(), installed
    queued = second.wait(timeout=300)
    assert queued.succeeded(), queued
    assert queued.result("install") == {"skipped": [INSTALL_TEST_PACKAGE]}, queued


@pytest.mark.bootstrapped
def test_two_users_change_their_profiles_at_the_same_time(
    container, mock_nix_server, mirror_cache
):
    create_user(container, SECOND_USER, sudo=True)
    bootstrap_as(container, SECOND_USER, mock_nix_server, mirror_cache)
    first = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    build = _frozen_build(container, USER)

    other = container.mix("install", INSTALL_TEST_PACKAGE, user=SECOND_USER)

    assert other.succeeded(), other
    assert not [e for e in other.envelopes if "lockWait" in e.get("nodeStarted", {})]
    container.exec("kill", "-CONT", build, check=True)
    assert first.wait(timeout=300).succeeded()


@pytest.mark.bootstrapped
def test_collecting_the_store_waits_for_a_running_install(
    container, mock_nix_server, mirror_cache
):
    first = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    build = _frozen_build(container, USER)

    clean = container.mix_background("clean", "--all", user=USER)
    wait = _lock_wait(clean)
    assert wait["lock"] == "/var/lib/mix/lock", wait
    assert (wait.get("holder"), wait.get("command")) == (USER, "install"), wait

    container.exec("kill", "-CONT", build, check=True)
    assert first.wait(timeout=300).succeeded()
    cleaned = clean.wait(timeout=300)
    assert cleaned.succeeded(), cleaned
