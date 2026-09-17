from conftest import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, create_user

USER = MIRROR_TEST_USERS[0]


def _bootstrap_as(container, user, mock_nix_server, mirror_cache):
    create_user(container, user, sudo=True)
    mirror_key = (mirror_cache / "mix-mirror.pub").read_text().strip()
    result = container.exec(
        "mix",
        "bootstrap",
        "--mirror",
        mock_nix_server["url"],
        "--mirror-key",
        mirror_key,
        user=user,
    )
    assert result.returncode == 0, result.stderr
    return result


def test_install_adds_a_package_as_a_regular_user_with_no_sudo(
    container, mock_nix_server, mirror_cache
):
    _bootstrap_as(container, USER, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"

    result = container.exec("mix", "install", INSTALL_TEST_PACKAGE, user=USER)

    assert result.returncode == 0, result.stderr
    assert INSTALL_TEST_PACKAGE in result.stdout.lower()
    assert container.path_exists(f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}")

    state = container.exec("cat", f"{state_dir}/state", check=True).stdout
    assert INSTALL_TEST_PACKAGE in state
    assert "git" in state

    home_nix = container.exec("cat", f"{state_dir}/home.nix", check=True).stdout
    assert INSTALL_TEST_PACKAGE in home_nix

    git_bin = f"/home/{USER}/.nix-profile/bin/git"
    log = container.exec(
        git_bin, "-C", state_dir, "log", "--format=%an <%ae>", user=USER, check=True
    )
    assert log.stdout.strip().splitlines()[0] == "mix <mix@localhost>"
    status = container.exec(git_bin, "-C", state_dir, "status", "--short", user=USER, check=True)
    assert status.stdout.strip() == ""


def test_install_rejects_a_package_that_is_already_installed(
    container, mock_nix_server, mirror_cache
):
    _bootstrap_as(container, USER, mock_nix_server, mirror_cache)

    result = container.exec("mix", "install", "git", user=USER)

    assert result.returncode != 0
    assert "already installed" in (result.stdout + result.stderr).lower()


def test_install_cannot_be_run_as_root(container, mock_nix_server, mirror_cache):
    _bootstrap_as(container, USER, mock_nix_server, mirror_cache)

    result = container.exec("mix", "install", INSTALL_TEST_PACKAGE)

    assert result.returncode != 0
    assert "cannot be run as root" in (result.stdout + result.stderr).lower()
    assert not container.path_exists(f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}")
