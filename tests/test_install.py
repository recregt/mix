import json

from conftest import (
    INSTALL_TEST_PACKAGE,
    MIRROR_TEST_USERS,
    UNCACHED_TEST_PACKAGE,
    bootstrap_as,
    create_user,
    mirror_args,
)

USER = MIRROR_TEST_USERS[0]


def _bootstrapped(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    bootstrap_as(container, USER, mock_nix_server, mirror_cache)


def test_install_adds_a_package_as_a_regular_user_with_no_sudo(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"
    mirror = mirror_args(mock_nix_server, mirror_cache)

    result = container.exec("mix", "install", INSTALL_TEST_PACKAGE, *mirror, user=USER)

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

    # Re-running the same install is a no-op that still succeeds, so a script can install
    # unconditionally.
    again = container.exec("mix", "install", INSTALL_TEST_PACKAGE, *mirror, user=USER)
    assert again.returncode == 0, again.stderr
    assert "already installed" in (again.stdout + again.stderr).lower()
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == state


def test_install_skips_a_package_that_is_already_installed(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"
    state_before = container.exec("cat", f"{state_dir}/state", check=True).stdout

    result = container.exec(
        "mix", "install", "git", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )

    assert result.returncode == 0, result.stderr
    assert "already installed" in (result.stdout + result.stderr).lower()
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == state_before


def test_install_is_script_friendly(container, mock_nix_server, mirror_cache):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    mirror = mirror_args(mock_nix_server, mirror_cache)

    result = container.exec(
        "mix", "install", "--json", INSTALL_TEST_PACKAGE, "git", *mirror, user=USER
    )

    # Stdout is the report and nothing else, so a script never has to parse prose.
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {
        "added": [INSTALL_TEST_PACKAGE],
        "skipped": ["git"],
    }

    again = container.exec("mix", "install", "--json", INSTALL_TEST_PACKAGE, *mirror, user=USER)
    assert again.returncode == 0, again.stderr
    assert json.loads(again.stdout) == {"added": [], "skipped": [INSTALL_TEST_PACKAGE]}

    # Plain output can also be demanded explicitly, with or without JSON.
    for args, env in (
        (["--no-progress", "install", INSTALL_TEST_PACKAGE, *mirror], None),
        (["install", INSTALL_TEST_PACKAGE, *mirror], {"CI": "true"}),
        (["install", INSTALL_TEST_PACKAGE, *mirror], {"MIX_NO_PROGRESS": "1"}),
    ):
        plain = container.exec("mix", *args, user=USER, env=env)
        assert plain.returncode == 0, plain.stderr
        assert "already installed" in (plain.stdout + plain.stderr).lower()
        assert "\x1b[" not in plain.stderr, "nothing should be drawn in place"


def test_install_cannot_be_run_as_root(container, mock_nix_server, mirror_cache):
    _bootstrapped(container, mock_nix_server, mirror_cache)

    result = container.exec("mix", "install", INSTALL_TEST_PACKAGE)

    assert result.returncode != 0
    assert "cannot be run as root" in (result.stdout + result.stderr).lower()
    assert not container.path_exists(f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}")


def test_install_refuses_a_package_the_cache_cannot_serve(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"
    state_before = container.exec("cat", f"{state_dir}/state", check=True).stdout
    home_before = container.exec("cat", f"{state_dir}/home.nix", check=True).stdout

    # The mirror is the only substituter here, and it was never given this package, so
    # installing it could only mean compiling it.
    result = container.exec(
        "mix",
        "install",
        UNCACHED_TEST_PACKAGE,
        *mirror_args(mock_nix_server, mirror_cache),
        user=USER,
    )

    assert result.returncode != 0
    output = (result.stdout + result.stderr).lower()
    assert "binary cache" in output
    assert "--build" in output
    assert UNCACHED_TEST_PACKAGE in output

    # Refusing costs the user nothing: the profile and the files that describe it are untouched.
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == state_before
    assert container.exec("cat", f"{state_dir}/home.nix", check=True).stdout == home_before
    assert not container.path_exists(f"/home/{USER}/.nix-profile/bin/{UNCACHED_TEST_PACKAGE}")


def test_install_with_the_build_flag_installs_a_cached_package_as_usual(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)

    result = container.exec(
        "mix",
        "install",
        "--build",
        INSTALL_TEST_PACKAGE,
        *mirror_args(mock_nix_server, mirror_cache),
        user=USER,
    )

    assert result.returncode == 0, result.stderr
    assert container.path_exists(f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}")


def test_install_rolls_back_state_and_home_nix_when_activation_fails(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    state_dir = f"/home/{USER}/.local/state/mix"
    state_before = container.exec("cat", f"{state_dir}/state", check=True).stdout
    home_before = container.exec("cat", f"{state_dir}/home.nix", check=True).stdout

    result = container.exec(
        "mix",
        "install",
        "doesnotexistinnixpkgs",
        *mirror_args(mock_nix_server, mirror_cache),
        user=USER,
    )

    assert result.returncode != 0
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == state_before
    assert container.exec("cat", f"{state_dir}/home.nix", check=True).stdout == home_before
