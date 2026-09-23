from conftest import (
    MIX_USERS_GROUP,
    NIX_CONF_CONTENT,
    create_user,
    daemon_trusts,
    group_members,
)


def test_bootstrap_accepts_mirror_as_a_cli_flag(container, mock_nix_server):
    result = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stderr
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_bootstrap_is_silent_by_default(container, mock_nix_server):
    result = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stderr
    assert "running command" not in result.stderr.lower()


def test_bootstrap_verbose_shows_command_execution(container, mock_nix_server):
    result = container.exec(
        "mix", "-vv", "bootstrap", "--mirror", mock_nix_server["url"]
    )

    assert result.returncode == 0, result.stderr
    assert "running command" in result.stderr.lower()


def test_bootstrap_mirror_flag_takes_precedence_over_the_env_var(container, mock_nix_server):
    result = container.exec(
        "mix",
        "bootstrap",
        "--mirror",
        mock_nix_server["url"],
        env={"MIX_NIX_MIRROR": "http://invalid.internal"},
    )

    assert result.returncode == 0, result.stderr
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_bootstrap_fails_cleanly_with_an_unreachable_mirror(container):
    result = container.exec("mix", "bootstrap", "--mirror", "http://127.0.0.1:1")

    assert result.returncode != 0
    assert "network" in (result.stdout + result.stderr).lower()
    assert not container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_bootstrap_auto_escalates_for_a_sudo_user(container, mock_nix_server, mirror_cache):
    create_user(container, "ciuser", sudo=True)

    mirror_key = (mirror_cache / "mix-mirror.pub").read_text().strip()
    result = container.exec(
        "mix",
        "bootstrap",
        "--mirror",
        mock_nix_server["url"],
        "--mirror-key",
        mirror_key,
        user="ciuser",
    )

    assert result.returncode == 0, result.stderr
    assert "re-running with sudo" in result.stderr.lower()
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")

    state_dir = "/home/ciuser/.local/state/mix"
    flake_nix = f"{state_dir}/flake.nix"
    home_nix = f"{state_dir}/home.nix"
    assert container.path_exists(flake_nix)
    assert container.path_exists(home_nix)

    owner = container.exec("stat", "-c", "%U:%G", state_dir, check=True)
    assert owner.stdout.strip() == "ciuser:ciuser"
    flake_owner = container.exec("stat", "-c", "%U:%G", flake_nix, check=True)
    assert flake_owner.stdout.strip() == "ciuser:ciuser"

    flake_contents = container.exec("cat", flake_nix, check=True).stdout
    assert 'system = "x86_64-linux"' in flake_contents
    assert 'homeConfigurations."ciuser"' in flake_contents

    assert container.exec("cat", "/etc/nix/nix.conf", check=True).stdout == NIX_CONF_CONTENT
    assert group_members(container, MIX_USERS_GROUP) == ["ciuser"]
    assert daemon_trusts(container, "ciuser")

    git_dir = f"{state_dir}/.git"
    assert container.path_exists(git_dir)
    git_bin = "/home/ciuser/.nix-profile/bin/git"
    log = container.exec(git_bin, "-C", state_dir, "log", "--format=%an <%ae>", user="ciuser", check=True)
    assert log.stdout.strip() == "mix <mix@localhost>"
    status = container.exec(git_bin, "-C", state_dir, "status", "--short", user="ciuser", check=True)
    assert status.stdout.strip() == ""

    doctor = container.exec("mix", "doctor", user="ciuser")
    assert doctor.returncode == 0, doctor.stdout + doctor.stderr

    stray = f"{state_dir}/id_rsa"
    container.exec("bash", "-c", f"echo stray > {stray}", check=True)
    container.exec("chown", "ciuser:ciuser", stray, check=True)

    synced = container.exec("mix", "repair", user="ciuser")
    assert synced.returncode == 0, synced.stderr

    tracked = set(
        container.exec(git_bin, "-C", state_dir, "ls-files", user="ciuser", check=True).stdout.split()
    )
    assert {".gitignore", "flake.nix", "home.nix"} <= tracked
    assert "id_rsa" not in tracked, "mix must not track a file it did not generate"
    assert container.path_exists(stray), "mix must not delete what it does not manage"
    ignored_status = container.exec(git_bin, "-C", state_dir, "status", "--short", user="ciuser", check=True)
    assert ignored_status.stdout.strip() == ""

    container.exec("chown", "-R", "root:root", state_dir, check=True)
    container.exec("chmod", "755", state_dir, check=True)

    drifted = container.exec("mix", "doctor", user="ciuser")
    assert drifted.returncode != 0, "doctor should detect the ownership/mode drift"

    fixed = container.exec("mix", "repair", user="ciuser")
    assert fixed.returncode == 0, fixed.stderr

    owner_after_repair = container.exec("stat", "-c", "%U:%G", state_dir, check=True)
    assert owner_after_repair.stdout.strip() == "ciuser:ciuser"
    mode_after_repair = container.exec("stat", "-c", "%a", state_dir, check=True)
    assert mode_after_repair.stdout.strip() == "700"

    healed = container.exec("mix", "doctor", user="ciuser")
    assert healed.returncode == 0, healed.stdout + healed.stderr

    container.exec("rm", "-rf", state_dir, check=True)
    assert group_members(container, MIX_USERS_GROUP) == ["ciuser"], (
        "group membership must survive deleting the state dir"
    )

    wiped = container.exec("mix", "doctor", user="ciuser")
    assert wiped.returncode != 0, "doctor should treat a wiped state dir as drift, not as unmanaged"

    restored = container.exec("mix", "repair", user="ciuser")
    assert restored.returncode == 0, restored.stderr

    assert container.path_exists(flake_nix)
    assert container.path_exists(home_nix)
    restored_owner = container.exec("stat", "-c", "%U:%G", flake_nix, check=True)
    assert restored_owner.stdout.strip() == "ciuser:ciuser"

    restored_flake_contents = container.exec("cat", flake_nix, check=True).stdout
    assert 'homeConfigurations."ciuser"' in restored_flake_contents
