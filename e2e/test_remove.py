
from support.container import create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, bootstrap_as

USER = MIRROR_TEST_USERS[0]
STATE_DIR = f"/home/{USER}/.local/state/mix"
PROFILE_BIN = f"/home/{USER}/.nix-profile/bin"


def _bootstrap_with_the_test_package(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    bootstrap_as(container, USER, mock_nix_server, mirror_cache)
    run = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert run.succeeded(), run
    assert container.path_exists(f"{PROFILE_BIN}/{INSTALL_TEST_PACKAGE}")


def _read(container, name):
    return container.exec("cat", f"{STATE_DIR}/{name}", check=True).stdout


def test_remove_drops_a_package_as_a_regular_user_with_no_sudo(
    container, mock_nix_server, mirror_cache
):
    _bootstrap_with_the_test_package(container, mock_nix_server, mirror_cache)

    run = container.mix("remove", INSTALL_TEST_PACKAGE, user=USER)

    assert run.succeeded(), run
    assert run.result("remove")["removed"] == [INSTALL_TEST_PACKAGE]
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
    status = container.exec(
        git_bin, "-C", STATE_DIR, "status", "--short", user=USER, check=True
    )
    assert status.stdout.strip() == ""

    again = container.mix("remove", INSTALL_TEST_PACKAGE, user=USER)
    assert again.succeeded(), again
    assert again.result("remove") == {"skipped": [INSTALL_TEST_PACKAGE]}
    assert _read(container, "state") == state
