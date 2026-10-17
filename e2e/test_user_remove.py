import pytest

from support.container import MIX_USERS_GROUP, NIX_CONF_DEST, create_user, group_members
from support.mirror import (
    INSTALL_TEST_PACKAGE,
    MIRROR_TEST_USERS,
    bootstrap_as,
    install_activation,
)

FIRST_USER, SECOND_USER = MIRROR_TEST_USERS
MACHINE_PATHS = ["/nix/store", NIX_CONF_DEST, "/etc/mix", "/var/lib/mix"]


def _exists(container, path: str) -> bool:
    return container.exec("test", "-e", path).returncode == 0


def _state(user: str) -> str:
    return f"/home/{user}/.local/state/mix"


@pytest.mark.bootstrapped
def test_a_user_removal_takes_that_user_and_leaves_the_machine_and_the_others(
    container, mock_nix_server, mirror_cache
):
    create_user(container, SECOND_USER, sudo=True)
    bootstrap_as(container, SECOND_USER, mock_nix_server, mirror_cache)
    for user in (FIRST_USER, SECOND_USER):
        installed = container.mix("install", INSTALL_TEST_PACKAGE, user=user)
        assert installed.succeeded(), installed

    removed = container.mix("user", "remove", "--yes", user=FIRST_USER)

    assert removed.succeeded(), removed
    assert removed.document["command"] == "user remove"
    result = removed.result("userRemove")
    assert result["user"] == FIRST_USER, result
    assert result["others"] == [SECOND_USER], result
    assert group_members(container, MIX_USERS_GROUP) == [SECOND_USER]
    assert not _exists(container, _state(FIRST_USER))
    assert not _exists(container, f"/home/{FIRST_USER}/.nix-profile")
    assert _exists(container, _state(SECOND_USER))
    for path in MACHINE_PATHS:
        assert _exists(container, path), path
    doctor = container.mix("doctor", user=SECOND_USER)
    assert doctor.succeeded(), doctor
    installed = container.mix("install", INSTALL_TEST_PACKAGE, user=SECOND_USER)
    assert installed.succeeded(), installed


@pytest.mark.bootstrapped
def test_the_last_user_to_leave_sees_that_nobody_else_remains_and_a_purge_finishes_the_job(
    container,
):
    removed = container.mix("user", "remove", "--yes", user=FIRST_USER)

    assert removed.succeeded(), removed
    result = removed.result("userRemove")
    assert result["user"] == FIRST_USER, result
    assert not result.get("others"), result
    assert group_members(container, MIX_USERS_GROUP) == []
    for path in MACHINE_PATHS:
        assert _exists(container, path), path

    purged = container.mix("purge", "--yes")

    assert purged.succeeded(), purged
    assert purged.document["command"] == "purge"
    for path in MACHINE_PATHS:
        assert not _exists(container, path), path


@pytest.mark.bootstrapped
def test_a_user_removal_without_a_terminal_or_yes_is_refused_and_changes_nothing(container):
    refused = container.mix("user", "remove", user=FIRST_USER)

    assert refused.returncode == 2, refused
    assert refused.code == "CODE_CONFIRMATION_REQUIRED", refused
    assert refused.changes == [], refused
    assert _exists(container, _state(FIRST_USER))
    assert group_members(container, MIX_USERS_GROUP) == [FIRST_USER]


@pytest.mark.bootstrapped
def test_a_dry_run_of_a_user_removal_makes_no_change(container):
    planned = container.mix("user", "remove", "--dry-run", user=FIRST_USER)

    assert planned.succeeded(), planned
    assert planned.document["dryRun"], planned
    assert _exists(container, _state(FIRST_USER))
    assert group_members(container, MIX_USERS_GROUP) == [FIRST_USER]
    doctor = container.mix("doctor", user=FIRST_USER)
    assert doctor.succeeded(), doctor


def _on_a_terminal(container, command: str, answers: str, user: str):
    """Runs `command` with a terminal as its stdin and stderr, typing `answers` into it."""
    typed = answers.replace("\n", "\\n")
    return container.exec(
        "sh",
        "-c",
        f"printf '{typed}' | script -qec '{command}' /dev/null",
        user=user,
    )


@pytest.mark.bootstrapped
def test_the_last_user_on_a_terminal_is_offered_the_machine_and_a_yes_purges_it(container):
    run = _on_a_terminal(container, "mix user remove", "y\ny\n", FIRST_USER)

    assert run.returncode == 0, run
    assert "This removes `mix` and Nix from your account. Continue? [y/N]" in run.stdout, run
    assert "Remove Nix from this machine as well? [y/N]" in run.stdout, run
    assert not _exists(container, _state(FIRST_USER))
    for path in MACHINE_PATHS:
        assert not _exists(container, path), path


@pytest.mark.bootstrapped
def test_the_last_user_who_says_no_keeps_the_machine(container):
    run = _on_a_terminal(container, "mix user remove", "y\nn\n", FIRST_USER)

    assert run.returncode == 0, run
    assert "Remove Nix from this machine as well? [y/N]" in run.stdout, run
    assert not _exists(container, _state(FIRST_USER))
    for path in MACHINE_PATHS:
        assert _exists(container, path), path


@pytest.mark.bootstrapped
def test_a_user_who_left_is_refused_by_the_daemon(container):
    removed = container.mix("user", "remove", "--yes", user=FIRST_USER)
    assert removed.succeeded(), removed

    listed = container.mix("list", user=FIRST_USER)

    assert not listed.succeeded(), listed
    assert listed.returncode != 0, listed


@pytest.mark.bootstrapped
def test_a_user_can_leave_while_another_users_command_is_running(
    container, mock_nix_server, mirror_cache
):
    create_user(container, SECOND_USER, sudo=True)
    bootstrap_as(container, SECOND_USER, mock_nix_server, mirror_cache)
    gate = container.gate(install_activation(mirror_cache))
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=FIRST_USER)
    gate.reached(unless=install)

    removed = container.mix("user", "remove", "--yes", user=SECOND_USER)

    assert removed.succeeded(), removed
    assert group_members(container, MIX_USERS_GROUP) == [FIRST_USER]
    gate.release()
    installed = install.wait()
    assert installed.succeeded(), installed
