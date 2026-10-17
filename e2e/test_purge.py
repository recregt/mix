import pytest

from support.container import MIX_USERS_GROUP, NIX_CONF_DEST, create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, bootstrap_as

FIRST_USER, SECOND_USER = MIRROR_TEST_USERS
UNIT_FILES = [
    f"/etc/systemd/system/{name}"
    for name in (
        "nix-daemon.socket",
        "nix-daemon.service",
        "mix-daemon.socket",
        "mix-daemon.service",
    )
]
MACHINE_PATHS = [
    "/nix/store",
    "/nix/var",
    NIX_CONF_DEST,
    "/etc/profile.d/mix-nix.sh",
    "/etc/mix",
    "/var/lib/mix",
    *UNIT_FILES,
]


def _exists(container, path: str) -> bool:
    return container.exec("test", "-e", path).returncode == 0


def _links_into_nix(container, user: str) -> str:
    return container.exec("find", f"/home/{user}", "-lname", "/nix/*", check=True, user=user).stdout


@pytest.mark.bootstrapped
def test_a_purge_leaves_no_trace_of_mix_on_the_machine_or_in_any_home(
    container, mock_nix_server, mirror_cache
):
    create_user(container, SECOND_USER, sudo=True)
    bootstrap_as(container, SECOND_USER, mock_nix_server, mirror_cache)
    for user in (FIRST_USER, SECOND_USER):
        installed = container.mix("install", INSTALL_TEST_PACKAGE, user=user)
        assert installed.succeeded(), installed
        assert _links_into_nix(container, user), f"{user} has nothing linked into /nix"

    removed = container.mix("purge", "--yes", user=FIRST_USER)

    assert removed.succeeded(), removed
    assert removed.document["command"] == "purge"
    assert sorted(removed.result("purge")["users"]) == sorted([FIRST_USER, SECOND_USER])
    assert set(removed.warnings) <= {"CODE_CLEANUP_INCOMPLETE"}, removed.warnings
    left = [path for path in MACHINE_PATHS if _exists(container, path)]
    assert left == [], left
    for unit in ("nix-daemon.socket", "mix-daemon.socket", "mix-daemon.service"):
        active = container.exec("systemctl", "is-active", unit).stdout.strip()
        assert active != "active", f"{unit} is still {active}"
    for group in ("nixbld", MIX_USERS_GROUP):
        assert container.exec("getent", "group", group).returncode != 0, group
    assert container.exec("getent", "passwd", "nixbld1").returncode != 0
    for user in (FIRST_USER, SECOND_USER):
        assert not _exists(container, f"/home/{user}/.local/state/mix"), user
        assert not _exists(container, f"/home/{user}/.nix-profile"), user
        assert _links_into_nix(container, user) == "", user


@pytest.mark.bootstrapped
def test_a_purge_without_a_terminal_or_yes_is_refused_and_changes_nothing(container):
    refused = container.mix("purge", user=FIRST_USER)

    assert refused.returncode == 2, refused
    assert refused.code == "CODE_CONFIRMATION_REQUIRED", refused
    assert refused.changes == [], refused
    for path in ("/nix/store", NIX_CONF_DEST, *UNIT_FILES):
        assert _exists(container, path), path


@pytest.mark.bootstrapped
def test_a_dry_run_of_a_purge_lists_the_changes_and_makes_none(container):
    planned = container.mix("purge", "--dry-run", user=FIRST_USER)

    assert planned.succeeded(), planned
    assert planned.document["dryRun"], planned
    assert planned.changes, planned
    updates = {
        change["operation"]
        for change in planned.changes
        if change["action"] == "CHANGE_ACTION_UPDATE"
    }
    assert updates <= {
        "OPERATION_STOP_UNIT",
        "OPERATION_DISABLE_UNIT",
        "OPERATION_DAEMON_RELOAD",
    }, updates
    assert {change["action"] for change in planned.changes} <= {
        "CHANGE_ACTION_DELETE",
        "CHANGE_ACTION_UPDATE",
    }, planned.changes
    for path in ("/nix/store", NIX_CONF_DEST, f"/home/{FIRST_USER}/.local/state/mix"):
        assert _exists(container, path), path
    doctor = container.mix("doctor", user=FIRST_USER)
    assert doctor.succeeded(), doctor
