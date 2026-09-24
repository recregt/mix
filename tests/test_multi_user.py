import shlex

from conftest import (
    MIRROR_TEST_USERS,
    MIX_USERS_GROUP,
    NIX_CONF_CONTENT,
    NIX_CONF_DEST,
    bootstrap_as,
    bootstrap_root,
    create_user,
    daemon_trusts,
    group_members,
)

FIRST_USER, SECOND_USER = MIRROR_TEST_USERS
UNMANAGED_USER = "plainuser"


def _nix_conf(container):
    return container.exec("cat", NIX_CONF_DEST, check=True).stdout


def test_every_managed_user_is_trusted_through_the_group(
    container, mock_nix_server, mirror_cache
):
    create_user(container, FIRST_USER, sudo=True)
    create_user(container, SECOND_USER, sudo=True)
    create_user(container, UNMANAGED_USER)

    bootstrap_as(container, FIRST_USER, mock_nix_server, mirror_cache)
    assert _nix_conf(container) == NIX_CONF_CONTENT
    assert group_members(container, MIX_USERS_GROUP) == [FIRST_USER]
    assert daemon_trusts(container, FIRST_USER)

    bootstrap_as(container, SECOND_USER, mock_nix_server, mirror_cache)

    assert _nix_conf(container) == NIX_CONF_CONTENT, (
        "enrolling a second user must not touch nix.conf"
    )
    assert group_members(container, MIX_USERS_GROUP) == sorted([FIRST_USER, SECOND_USER])
    assert daemon_trusts(container, FIRST_USER), "the first user must not lose trust"
    assert daemon_trusts(container, SECOND_USER)
    assert not daemon_trusts(container, UNMANAGED_USER)

    for user in (FIRST_USER, SECOND_USER):
        doctor = container.exec("mix", "doctor", user=user)
        assert doctor.returncode == 0, doctor.stdout + doctor.stderr

    repair = container.exec("mix", "repair", user=SECOND_USER)
    assert repair.returncode == 0, repair.stderr

    assert _nix_conf(container) == NIX_CONF_CONTENT
    assert group_members(container, MIX_USERS_GROUP) == sorted([FIRST_USER, SECOND_USER])
    assert daemon_trusts(container, FIRST_USER), (
        "one user's repair must not revoke another's trust"
    )

    doctor = container.exec("mix", "doctor", user=FIRST_USER)
    assert doctor.returncode == 0, doctor.stdout + doctor.stderr


def test_removing_a_user_from_the_group_revokes_trust_without_touching_nix_conf(
    container, mock_nix_server, mirror_cache
):
    create_user(container, FIRST_USER, sudo=True)
    bootstrap_as(container, FIRST_USER, mock_nix_server, mirror_cache)
    assert daemon_trusts(container, FIRST_USER)

    container.exec("gpasswd", "--delete", FIRST_USER, MIX_USERS_GROUP, check=True)

    assert not daemon_trusts(container, FIRST_USER)
    assert _nix_conf(container) == NIX_CONF_CONTENT

    state_dir = f"/home/{FIRST_USER}/.local/state/mix"
    assert container.path_exists(f"{state_dir}/flake.nix")

    doctor = container.exec("mix", "doctor", user=FIRST_USER)
    assert doctor.returncode == 0, (
        "an un-enrolled user is unmanaged, not drifted: " + doctor.stdout + doctor.stderr
    )

    bootstrap_as(container, FIRST_USER, mock_nix_server, mirror_cache)

    assert group_members(container, MIX_USERS_GROUP) == [FIRST_USER]
    assert daemon_trusts(container, FIRST_USER)


def test_a_rewritten_nix_conf_reaches_the_running_daemon(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)
    create_user(container, UNMANAGED_USER)

    legacy_conf = NIX_CONF_CONTENT.replace(
        f"trusted-users = root @{MIX_USERS_GROUP}",
        f"trusted-users = root {UNMANAGED_USER}",
    )
    container.exec(
        "bash",
        "-c",
        f"printf '%s' {shlex.quote(legacy_conf)} > {NIX_CONF_DEST}",
        check=True,
    )
    container.exec("systemctl", "restart", "nix-daemon.service", check=True)
    assert daemon_trusts(container, UNMANAGED_USER), (
        "the daemon must be running with the legacy config for this test to mean anything"
    )

    assert container.exec("mix", "doctor").returncode != 0

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert _nix_conf(container) == NIX_CONF_CONTENT
    assert not daemon_trusts(container, UNMANAGED_USER)


def test_bootstrapping_as_bare_root_enrols_nobody(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    assert container.exec("getent", "group", MIX_USERS_GROUP).returncode == 0
    assert group_members(container, MIX_USERS_GROUP) == []
    assert _nix_conf(container) == NIX_CONF_CONTENT
    assert not container.path_exists("/root/.local/state/mix")

    doctor = container.exec("mix", "doctor")
    assert doctor.returncode == 0, doctor.stdout + doctor.stderr
    assert daemon_trusts(container, "root")
