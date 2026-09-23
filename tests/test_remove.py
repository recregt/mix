import json

from conftest import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, bootstrap_as, create_user, mirror_args

USER = MIRROR_TEST_USERS[0]
STATE_DIR = f"/home/{USER}/.local/state/mix"
PROFILE_BIN = f"/home/{USER}/.nix-profile/bin"


def _bootstrap_with_the_test_package(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    bootstrap_as(container, USER, mock_nix_server, mirror_cache)
    mirror = mirror_args(mock_nix_server, mirror_cache)
    result = container.exec("mix", "install", INSTALL_TEST_PACKAGE, *mirror, user=USER)
    assert result.returncode == 0, result.stderr
    assert container.path_exists(f"{PROFILE_BIN}/{INSTALL_TEST_PACKAGE}")
    return mirror


def _read(container, name):
    return container.exec("cat", f"{STATE_DIR}/{name}", check=True).stdout


def test_remove_drops_a_package_as_a_regular_user_with_no_sudo(
    container, mock_nix_server, mirror_cache
):
    mirror = _bootstrap_with_the_test_package(container, mock_nix_server, mirror_cache)

    result = container.exec("mix", "remove", INSTALL_TEST_PACKAGE, *mirror, user=USER)

    assert result.returncode == 0, result.stderr
    assert INSTALL_TEST_PACKAGE in (result.stdout + result.stderr).lower()
    assert not container.path_exists(f"{PROFILE_BIN}/{INSTALL_TEST_PACKAGE}")
    assert container.path_exists(f"{PROFILE_BIN}/git")

    state = _read(container, "state")
    assert INSTALL_TEST_PACKAGE not in state
    assert "git" in state
    assert INSTALL_TEST_PACKAGE not in _read(container, "home.nix")

    git_bin = f"{PROFILE_BIN}/git"
    log = container.exec(
        git_bin, "-C", STATE_DIR, "log", "--format=%an <%ae>", user=USER, check=True
    )
    assert log.stdout.strip().splitlines()[0] == "mix <mix@localhost>"
    status = container.exec(git_bin, "-C", STATE_DIR, "status", "--short", user=USER, check=True)
    assert status.stdout.strip() == ""

    again = container.exec("mix", "remove", INSTALL_TEST_PACKAGE, *mirror, user=USER)
    assert again.returncode == 0, again.stderr
    assert "not installed" in (again.stdout + again.stderr).lower()
    assert _read(container, "state") == state


def test_remove_is_script_friendly(container, mock_nix_server, mirror_cache):
    mirror = _bootstrap_with_the_test_package(container, mock_nix_server, mirror_cache)

    result = container.exec(
        "mix", "remove", "--json", INSTALL_TEST_PACKAGE, "notinstalled", *mirror, user=USER
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {
        "removed": [INSTALL_TEST_PACKAGE],
        "skipped": ["notinstalled"],
    }

    again = container.exec("mix", "remove", "--json", INSTALL_TEST_PACKAGE, *mirror, user=USER)
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout) == {"removed": [], "skipped": [INSTALL_TEST_PACKAGE]}

    plain = container.exec("mix", "--no-progress", "remove", INSTALL_TEST_PACKAGE, *mirror, user=USER)
    assert plain.returncode == 0, plain.stderr
    assert "not installed" in (plain.stdout + plain.stderr).lower()
    assert "\x1b[" not in plain.stderr, "nothing should be drawn in place"


def test_remove_refuses_a_package_mix_relies_on(container, mock_nix_server, mirror_cache):
    mirror = _bootstrap_with_the_test_package(container, mock_nix_server, mirror_cache)
    state_before = _read(container, "state")
    home_before = _read(container, "home.nix")

    for packages in (["git"], [INSTALL_TEST_PACKAGE, "git"]):
        result = container.exec("mix", "remove", *packages, *mirror, user=USER)

        assert result.returncode != 0
        output = (result.stdout + result.stderr).lower()
        assert "cannot be removed" in output
        assert "git" in output
        assert _read(container, "state") == state_before
        assert _read(container, "home.nix") == home_before
        assert container.path_exists(f"{PROFILE_BIN}/git")
        assert container.path_exists(f"{PROFILE_BIN}/{INSTALL_TEST_PACKAGE}")


def test_remove_cannot_be_run_as_root(container, mock_nix_server, mirror_cache):
    _bootstrap_with_the_test_package(container, mock_nix_server, mirror_cache)

    result = container.exec("mix", "remove", INSTALL_TEST_PACKAGE)

    assert result.returncode != 0
    assert "cannot be run as root" in (result.stdout + result.stderr).lower()
    assert container.path_exists(f"{PROFILE_BIN}/{INSTALL_TEST_PACKAGE}")
