from support.container import MIX_USERS_GROUP, create_user, daemon_trusts, group_members
from support.mirror import nix_conf_content


def test_bootstrap_accepts_mirror_as_a_cli_flag(container, mock_nix_server):
    run = container.mix("bootstrap", "--mirror", mock_nix_server["url"])

    assert run.succeeded(), run
    assert "bootstrap" in run.root
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_bootstrap_auto_escalates_for_a_sudo_user(
    container, mock_nix_server, mirror_cache
):
    create_user(container, "ciuser", sudo=True)

    mirror_key = (mirror_cache / "mix-mirror.pub").read_text().strip()
    run = container.mix(
        "bootstrap",
        "--mirror",
        mock_nix_server["url"],
        "--mirror-key",
        mirror_key,
        user="ciuser",
    )

    assert run.succeeded(), run
    assert "write-home-config" in run.steps()
    builds = [
        command["line"]
        for command in run.progress("command")
        if " build " in command["line"] and "/.local/state/mix" in command["line"]
    ]
    assert builds and all(
        "path:/home/ciuser/.local/state/mix#" in line for line in builds
    ), builds
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
    assert "pkgs = nixpkgs.legacyPackages.x86_64-linux;" in flake_contents
    assert "ciuser = home-manager.lib.homeManagerConfiguration {" in flake_contents

    assert container.exec(
        "cat", "/etc/nix/nix.conf", check=True
    ).stdout == nix_conf_content(mock_nix_server["url"], mirror_key)
    assert group_members(container, MIX_USERS_GROUP) == ["ciuser"]
    assert not daemon_trusts(container, "ciuser")

    daemon = container.exec(
        "stat", "-c", "%U:%G %a", "/var/lib/mix/bin/mix-daemon", check=True
    )
    assert daemon.stdout.strip() == "root:root 755"
    assert (
        container.exec(
            "cmp", "/usr/local/bin/mix-daemon", "/var/lib/mix/bin/mix-daemon"
        ).returncode
        == 0
    )
    socket = container.exec("systemctl", "is-active", "mix-daemon.socket")
    assert socket.stdout.strip() == "active", socket
    assert container.exec("test", "-S", "/run/mix/daemon.sock").returncode == 0

    git_dir = f"{state_dir}/.git"
    assert container.path_exists(git_dir)
    git_bin = "/home/ciuser/.nix-profile/bin/git"
    log = container.exec(
        git_bin, "-C", state_dir, "log", "--format=%an <%ae>", user="ciuser", check=True
    )
    assert log.stdout.strip() == "mix <mix@localhost>"
    status = container.exec(
        git_bin, "-C", state_dir, "status", "--short", user="ciuser", check=True
    )
    assert status.stdout.strip() == ""

    doctor = container.mix("doctor", user="ciuser")
    assert doctor.succeeded(), doctor
    assert all("finding" not in report for report in doctor.result("doctor")["reports"])
