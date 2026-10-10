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
    bootstrap_as,
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
        installed = container.mix("install", INSTALL_TEST_PACKAGE, user=user)
        assert installed.result("install")["added"] == [INSTALL_TEST_PACKAGE], installed
        doctor = container.mix("doctor", user=user)
        assert doctor.succeeded(), doctor

    repair = container.mix("repair", user=SECOND_USER)
    assert repair.succeeded(), repair
    assert "reports" not in repair.result("repair"), repair

    assert _nix_conf(container) == expected
    assert group_members(container, MIX_USERS_GROUP) == sorted([FIRST_USER, SECOND_USER])
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
    container.exec("bash", "-c", f"printf '%s' {shlex.quote(legacy)} > {NIX_CONF_DEST}", check=True)
    container.exec("systemctl", "restart", "nix-daemon.service", check=True)
    assert daemon_trusts(container, FIRST_USER), (
        "the daemon must be running with the legacy config for this test to mean anything"
    )

    doctor = container.mix("doctor", user=FIRST_USER)
    assert doctor.exit_code == 3, doctor
    drifted = [report for report in doctor.result("doctor")["reports"] if "finding" in report]
    assert [report["target"] for report in drifted] == [NIX_CONF_DEST], doctor

    repair = container.mix("repair", user=FIRST_USER)
    assert repair.succeeded(), repair
    fixed = [
        report["target"] for report in repair.result("repair")["reports"] if report.get("fixed")
    ]
    assert NIX_CONF_DEST in fixed, repair

    assert _nix_conf(container) == expected
    assert not daemon_trusts(container, FIRST_USER)
