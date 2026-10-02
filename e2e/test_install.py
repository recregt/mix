import pytest
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]


@pytest.mark.bootstrapped
def test_install_adds_a_package_as_a_regular_user_with_no_sudo(
    container, mock_nix_server, mirror_cache
):
    state_dir = f"/home/{USER}/.local/state/mix"

    run = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)

    assert run.succeeded(), run
    assert run.result("install")["added"] == [INSTALL_TEST_PACKAGE]
    builds = [
        command["line"]
        for command in run.progress("command")
        if " build " in command["line"]
    ]
    assert builds and all(f"git+file://{state_dir}#" in line for line in builds), builds
    assert container.path_exists(
        f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"
    )

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
    status = container.exec(
        git_bin, "-C", state_dir, "status", "--short", user=USER, check=True
    )
    assert status.stdout.strip() == ""

    again = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert again.succeeded(), again
    assert again.result("install") == {"skipped": [INSTALL_TEST_PACKAGE]}
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == state


@pytest.mark.bootstrapped
def test_an_unknown_package_is_named_and_nothing_changes(
    container, mock_nix_server, mirror_cache
):
    state_dir = f"/home/{USER}/.local/state/mix"
    before = container.exec("cat", f"{state_dir}/state", check=True).stdout

    run = container.mix("install", "ripgrep2", user=USER)

    assert run.exit_code == 1, run
    assert run.code == "CODE_UNKNOWN_PACKAGE", run
    assert run.root["diagnostic"]["packages"]["packages"] == ["ripgrep2"], run
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == before
