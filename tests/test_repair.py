from conftest import (
    MIRROR_TEST_USERS,
    MIX_USERS_GROUP,
    NIX_CONF_CONTENT,
    bootstrap_as,
    bootstrap_root,
    create_user,
    group_members,
)

DEFAULT_PROFILE_BIN = "/nix/var/nix/profiles/default/bin"
SYSTEM_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
MIX_MANAGED_MARKER = "/nix/.mix-managed"
USER = MIRROR_TEST_USERS[0]


def _bootstrapped(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    bootstrap_as(container, USER, mock_nix_server, mirror_cache)


def test_repair_is_a_clean_no_op_on_a_healthy_system(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    repair = container.exec("mix", "repair")

    assert repair.returncode == 0, repair.stderr
    assert "nothing to repair" in repair.stdout.lower()


def test_repair_fixes_drift_with_a_clean_shell_path(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)
    container.exec("groupmod", "--gid", "9999", "nixbld")

    repair = container.exec("mix", "repair", env={"PATH": SYSTEM_PATH})

    assert repair.returncode == 0, repair.stderr
    assert container.exec("getent", "group", "nixbld").stdout.split(":")[2] == "30000"
    assert container.path_exists(MIX_MANAGED_MARKER)


def test_repair_fixes_drift_when_path_still_points_into_the_deleted_snippet(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)
    container.exec("groupmod", "--gid", "9999", "nixbld")

    polluted_path = f"{DEFAULT_PROFILE_BIN}:{SYSTEM_PATH}"
    repair = container.exec("mix", "repair", env={"PATH": polluted_path})

    assert repair.returncode == 0, repair.stderr
    assert container.exec("getent", "group", "nixbld").stdout.split(":")[2] == "30000"


def test_repair_fixes_a_nix_dir_permission_drift(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("chmod", "700", "/nix")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect /nix permission drift"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("stat", "-c", "%a", "/nix").stdout.strip() == "755"


def test_repair_fixes_a_deleted_profile_snippet(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("rm", "-f", "/etc/profile.d/mix-nix.sh")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a deleted profile snippet"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.path_exists("/etc/profile.d/mix-nix.sh")


def test_repair_fixes_a_wrong_nixbld_gid(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("groupmod", "--gid", "9999", "nixbld")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a nixbld group with the wrong gid"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("getent", "group", "nixbld").stdout.split(":")[2] == "30000"


def test_repair_fixes_a_wrong_nixbld_user_gid(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("usermod", "--gid", "0", "nixbld1", check=True)

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a nixbld1 user with the wrong gid"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("id", "-g", "nixbld1").stdout.strip() == "30000"


def test_repair_cannot_restore_a_deleted_default_profile_but_bootstrap_can(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("rm", "-rf", "/nix/var/nix/profiles/default")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a missing default profile"

    repair = container.exec("mix", "repair")
    assert repair.returncode != 0, "repair cannot reinstall the Nix runtime itself"
    assert "mix bootstrap" in (repair.stdout + repair.stderr).lower()
    assert not container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")

    fix = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])
    assert fix.returncode == 0, fix.stderr

    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")
    assert container.exec("mix", "doctor").returncode == 0


def test_repair_fixes_injected_drift(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("bash", "-c", "echo corrupted > /etc/nix/nix.conf")
    container.exec("userdel", "nixbld1")
    container.exec("systemctl", "stop", "nix-daemon.socket")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect the injected drift before any repair"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("cat", "/etc/nix/nix.conf").stdout == NIX_CONF_CONTENT
    assert container.exec("getent", "passwd", "nixbld1").returncode == 0
    assert container.exec("systemctl", "is-active", "nix-daemon.socket").stdout.strip() == "active"


def test_repair_fixes_independent_targets_even_when_one_target_fails(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("chmod", "700", "/nix/var/nix/userpool")
    container.exec("userdel", "nixbld1")
    container.exec("bash", "-c", "mv /usr/sbin/useradd /usr/sbin/useradd.disabled")

    repair = container.exec("mix", "repair")
    assert repair.returncode != 0, "expected the disabled useradd to leave nixbld1 unrepaired"

    assert container.exec("stat", "-c", "%a", "/nix/var/nix/userpool").stdout.strip() == "755", (
        "the userpool directory fix is independent of the failed user fix and should still apply"
    )
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")

    container.exec("bash", "-c", "mv /usr/sbin/useradd.disabled /usr/sbin/useradd")
    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("getent", "passwd", "nixbld1").returncode == 0


def test_repair_syncs_config_without_tracking_a_stray_file(container, mock_nix_server, mirror_cache):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"
    git_bin = f"/home/{USER}/.nix-profile/bin/git"

    stray = f"{state_dir}/id_rsa"
    container.exec("bash", "-c", f"echo stray > {stray}", check=True)
    container.exec("chown", f"{USER}:{USER}", stray, check=True)

    synced = container.exec("mix", "repair", user=USER)
    assert synced.returncode == 0, synced.stderr

    tracked = set(
        container.exec(git_bin, "-C", state_dir, "ls-files", user=USER, check=True).stdout.split()
    )
    assert {".gitignore", "flake.nix", "home.nix"} <= tracked
    assert "id_rsa" not in tracked, "mix must not track a file it did not generate"
    assert container.path_exists(stray), "mix must not delete what it does not manage"
    status = container.exec(git_bin, "-C", state_dir, "status", "--short", user=USER, check=True)
    assert status.stdout.strip() == ""


def test_repair_fixes_state_dir_ownership_and_mode_drift(container, mock_nix_server, mirror_cache):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"

    container.exec("chown", "-R", "root:root", state_dir, check=True)
    container.exec("chmod", "755", state_dir, check=True)

    drifted = container.exec("mix", "doctor", user=USER)
    assert drifted.returncode != 0, "doctor should detect the ownership/mode drift"

    fixed = container.exec("mix", "repair", user=USER)
    assert fixed.returncode == 0, fixed.stderr

    owner_after_repair = container.exec("stat", "-c", "%U:%G", state_dir, check=True)
    assert owner_after_repair.stdout.strip() == f"{USER}:{USER}"
    mode_after_repair = container.exec("stat", "-c", "%a", state_dir, check=True)
    assert mode_after_repair.stdout.strip() == "700"

    healed = container.exec("mix", "doctor", user=USER)
    assert healed.returncode == 0, healed.stdout + healed.stderr


def test_repair_restores_a_wiped_state_dir(container, mock_nix_server, mirror_cache):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"
    flake_nix = f"{state_dir}/flake.nix"
    home_nix = f"{state_dir}/home.nix"

    container.exec("rm", "-rf", state_dir, check=True)
    assert group_members(container, MIX_USERS_GROUP) == [USER], (
        "group membership must survive deleting the state dir"
    )

    wiped = container.exec("mix", "doctor", user=USER)
    assert wiped.returncode != 0, "doctor should treat a wiped state dir as drift, not as unmanaged"

    restored = container.exec("mix", "repair", user=USER)
    assert restored.returncode == 0, restored.stderr

    assert container.path_exists(flake_nix)
    assert container.path_exists(home_nix)
    restored_owner = container.exec("stat", "-c", "%U:%G", flake_nix, check=True)
    assert restored_owner.stdout.strip() == f"{USER}:{USER}"

    restored_flake_contents = container.exec("cat", flake_nix, check=True).stdout
    assert f'homeConfigurations."{USER}"' in restored_flake_contents
