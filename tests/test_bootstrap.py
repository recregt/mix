from conftest import (
    MIX_USERS_GROUP,
    NIX_CONF_CONTENT,
    bootstrap_root,
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


def test_bootstrap_is_idempotent(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    result = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stderr


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
