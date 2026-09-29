import shlex

from support.container import (
    MIX_USERS_GROUP,
    NIX_CONF_DEST,
    create_user,
    daemon_trusts,
    group_members,
)
from support.mirror import (
    INSTALL_TEST_PACKAGE,
    MIRROR_TEST_USERS,
    POLICY_FILE,
    bootstrap_as,
    bootstrap_root,
    mirror_key,
    nix_conf_content,
)

FIRST_USER, SECOND_USER = MIRROR_TEST_USERS
UNMANAGED_USER = "plainuser"


def _nix_conf(container):
    return container.exec("cat", NIX_CONF_DEST, check=True).stdout


def test_no_enrolled_user_is_trusted_and_every_one_installs_from_the_policy(
    container, mock_nix_server, mirror_cache
):
    expected = nix_conf_content(mock_nix_server["url"], mirror_key(mirror_cache))
    create_user(container, FIRST_USER, sudo=True)
    create_user(container, SECOND_USER, sudo=True)
    create_user(container, UNMANAGED_USER)

    bootstrap_as(container, FIRST_USER, mock_nix_server, mirror_cache)
    assert _nix_conf(container) == expected
    assert group_members(container, MIX_USERS_GROUP) == [FIRST_USER]
    assert not daemon_trusts(container, FIRST_USER)

    bootstrap_as(container, SECOND_USER, mock_nix_server, mirror_cache)

    assert _nix_conf(container) == expected, "enrolling a second user must not touch nix.conf"
    assert group_members(container, MIX_USERS_GROUP) == sorted([FIRST_USER, SECOND_USER])
    for user in (FIRST_USER, SECOND_USER, UNMANAGED_USER):
        assert not daemon_trusts(container, user), user

    for user in (FIRST_USER, SECOND_USER):
        installed = container.exec("mix", "install", INSTALL_TEST_PACKAGE, user=user)
        assert installed.returncode == 0, installed.stdout + installed.stderr
        doctor = container.exec("mix", "doctor", user=user)
        assert doctor.returncode == 0, doctor.stdout + doctor.stderr

    repair = container.exec("mix", "repair", user=SECOND_USER)
    assert repair.returncode == 0, repair.stderr

    assert _nix_conf(container) == expected
    assert group_members(container, MIX_USERS_GROUP) == sorted([FIRST_USER, SECOND_USER])
    assert not daemon_trusts(container, FIRST_USER)


def test_a_user_taken_out_of_the_group_is_unmanaged_until_enrolled_again(
    container, mock_nix_server, mirror_cache
):
    create_user(container, FIRST_USER, sudo=True)
    bootstrap_as(container, FIRST_USER, mock_nix_server, mirror_cache)
    expected = _nix_conf(container)

    container.exec("gpasswd", "--delete", FIRST_USER, MIX_USERS_GROUP, check=True)

    assert _nix_conf(container) == expected
    assert container.path_exists(f"/home/{FIRST_USER}/.local/state/mix/flake.nix")
    doctor = container.exec("mix", "doctor", user=FIRST_USER)
    assert doctor.returncode == 0, (
        "an un-enrolled user is unmanaged, not drifted: " + doctor.stdout + doctor.stderr
    )

    bootstrap_as(container, FIRST_USER, mock_nix_server, mirror_cache)

    assert group_members(container, MIX_USERS_GROUP) == [FIRST_USER]
    assert not daemon_trusts(container, FIRST_USER)


def test_a_legacy_group_trust_is_removed_and_reaches_the_running_daemon(
    container, mock_nix_server, mirror_cache
):
    create_user(container, FIRST_USER, sudo=True)
    bootstrap_as(container, FIRST_USER, mock_nix_server, mirror_cache)
    expected = _nix_conf(container)
    legacy = expected.replace(
        "trusted-users = root\n", f"trusted-users = root @{MIX_USERS_GROUP}\n"
    )
    container.exec(
        "bash", "-c", f"printf '%s' {shlex.quote(legacy)} > {NIX_CONF_DEST}", check=True
    )
    container.exec("systemctl", "restart", "nix-daemon.service", check=True)
    assert daemon_trusts(container, FIRST_USER), (
        "the daemon must be running with the legacy config for this test to mean anything"
    )

    assert container.exec("mix", "doctor", user=FIRST_USER).returncode != 0

    repair = container.exec("mix", "repair", user=FIRST_USER)
    assert repair.returncode == 0, repair.stdout + repair.stderr

    assert _nix_conf(container) == expected
    assert not daemon_trusts(container, FIRST_USER)


def test_a_missing_policy_is_repaired_to_the_default_which_trusts_less(
    container, mock_nix_server
):
    bootstrap_root(container, mock_nix_server)
    container.exec("rm", POLICY_FILE, check=True)

    assert container.exec("mix", "doctor").returncode != 0

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stdout + repair.stderr

    assert _nix_conf(container) == nix_conf_content()
    assert container.exec("mix", "doctor").returncode == 0


def test_bootstrapping_as_bare_root_enrols_nobody(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    assert container.exec("getent", "group", MIX_USERS_GROUP).returncode == 0
    assert group_members(container, MIX_USERS_GROUP) == []
    assert _nix_conf(container) == nix_conf_content(mock_nix_server["url"])
    assert not container.path_exists("/root/.local/state/mix")
