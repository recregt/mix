import shlex

import pytest

from support.container import create_user, until
from support.mirror import (
    INSTALL_TEST_PACKAGE,
    MIRROR_TEST_USERS,
    bootstrap_as,
    install_activation,
    mirror_args,
)

USER, SECOND_USER = MIRROR_TEST_USERS
PACKAGE_BIN = f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"


def test_install_works_in_the_same_session_as_bootstrap(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    mirror = shlex.join(mirror_args(mock_nix_server, mirror_cache))

    result = container.exec(
        "bash",
        "-c",
        f"mix --json bootstrap {mirror} > /tmp/bootstrap.json"
        f" && mix --json install {INSTALL_TEST_PACKAGE} > /tmp/install.json",
        user=USER,
    )

    bootstrapped = container.exec("cat", "/tmp/bootstrap.json").stdout
    assert container.recorded(0, bootstrapped, result.stderr).succeeded()
    installed = container.exec("cat", "/tmp/install.json").stdout
    installed = container.recorded(result.returncode, installed, result.stderr)
    assert installed.succeeded(), installed
    assert installed.result("install")["added"] == [INSTALL_TEST_PACKAGE]
    assert container.path_exists(PACKAGE_BIN)


def _waiting(container, count: int, unless) -> None:
    """Returns once `count` requests wait for a lock in the daemon; fails if `unless` ends."""
    wanted = f"{count} waiting for a lock"

    def reported() -> bool:
        status = container.exec(
            "systemctl", "show", "-P", "StatusText", "mix-daemon.service", check=True
        ).stdout.strip()
        return status.endswith(wanted)

    until(reported, f"{count} requests waiting for a lock", unless=unless)


@pytest.mark.bootstrapped
def test_the_same_users_next_command_waits_and_names_the_one_it_waits_for(
    container, mock_nix_server, mirror_cache
):
    gate = container.gate(install_activation(mirror_cache))
    first = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=first)

    second = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    _waiting(container, 1, unless=second)
    third = container.mix_background("doctor", user=USER)
    _waiting(container, 2, unless=third)
    third.signal("INT")
    stopped = third.wait()
    assert stopped.status == "STATUS_CANCELLED", stopped

    gate.release()
    installed = first.wait()
    assert installed.succeeded(), installed
    queued = second.wait()
    assert queued.succeeded(), queued
    assert queued.result("install") == {"skipped": [INSTALL_TEST_PACKAGE]}, queued
    assert queued.waits == [{"lock": f"user {USER}", "holder": USER, "command": "install"}], queued


@pytest.mark.bootstrapped
def test_two_users_change_their_profiles_at_the_same_time(container, mock_nix_server, mirror_cache):
    create_user(container, SECOND_USER, sudo=True)
    bootstrap_as(container, SECOND_USER, mock_nix_server, mirror_cache)
    gate = container.gate(install_activation(mirror_cache))
    first = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=first)

    other = container.mix("install", INSTALL_TEST_PACKAGE, user=SECOND_USER)

    assert other.succeeded(), other
    assert other.waits == [], other
    gate.release()
    assert first.wait().succeeded()


@pytest.mark.bootstrapped
def test_collecting_the_store_waits_for_a_running_install(container, mock_nix_server, mirror_cache):
    gate = container.gate(install_activation(mirror_cache))
    first = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)
    gate.reached(unless=first)

    clean = container.mix_background("clean", "--all", user=USER)
    _waiting(container, 1, unless=clean)

    gate.release()
    assert first.wait().succeeded()
    cleaned = clean.wait()
    assert cleaned.succeeded(), cleaned
    (wait,) = cleaned.waits
    assert wait["lock"] == "/var/lib/mix/lock", wait
    assert (wait.get("holder"), wait.get("command")) == (USER, "install"), wait
