import pytest

from support.container import MIX_USERS_GROUP, NIX_CONF_DEST, create_user, group_members
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, bootstrap_as

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
    result = removed.result("selfRemove")
    assert result["users"] == [FIRST_USER], result
    assert result["others"] == [SECOND_USER], result
    assert not result.get("machine"), result
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
    result = removed.result("selfRemove")
    assert result["users"] == [FIRST_USER], result
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
