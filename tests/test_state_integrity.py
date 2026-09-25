import json

from conftest import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, mirror_args
from test_install import _bootstrapped

USER = MIRROR_TEST_USERS[0]
STATE_DIR = f"/home/{USER}/.local/state/mix"
STATE = f"{STATE_DIR}/state"
PROFILE = f"/home/{USER}/.local/state/nix/profiles/home-manager"
PACKAGE_BIN = f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"


def _install(container, mock_nix_server, mirror_cache, *flags):
    return container.exec(
        "mix",
        *flags,
        "install",
        INSTALL_TEST_PACKAGE,
        *mirror_args(mock_nix_server, mirror_cache),
        user=USER,
    )


def _state(container) -> str:
    return container.exec("cat", STATE, check=True).stdout


def _generation_state(container) -> str:
    return container.exec("cat", f"{PROFILE}/mix-state", check=True).stdout


def _packages(raw: str) -> list[str]:
    return json.loads(raw)["packages"]


def _kill_during(container, mock_nix_server, mirror_cache, marker: str, env=None) -> None:
    proc = container.start_background(
        "mix",
        "-vv",
        "--no-progress",
        "install",
        INSTALL_TEST_PACKAGE,
        *mirror_args(mock_nix_server, mirror_cache),
        env=env,
        user=USER,
    )
    proc.wait_for_output(marker, timeout=120)
    pid = proc.pid()
    container.exec("bash", "-c", f"pkill -KILL -P {pid}; kill -KILL {pid}")
    proc.wait(timeout=60)


def _assert_consistent(container) -> None:
    assert _state(container) == _generation_state(container)
    installed = container.path_exists(PACKAGE_BIN)
    assert (INSTALL_TEST_PACKAGE in _packages(_state(container))) == installed


def test_the_active_generation_carries_the_exact_package_list(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    assert _generation_state(container) == _state(container)

    result = _install(container, mock_nix_server, mirror_cache)

    assert result.returncode == 0, result.stderr
    assert _packages(_generation_state(container)) == ["git", INSTALL_TEST_PACKAGE]
    assert _generation_state(container) == _state(container)
    assert "/nix/store/" in container.exec("readlink", "-f", PROFILE, check=True).stdout


def test_a_command_killed_before_the_switch_is_undone_by_the_next_one(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)

    _kill_during(container, mock_nix_server, mirror_cache, "--dry-run")
    assert INSTALL_TEST_PACKAGE in _packages(_state(container))
    assert not container.path_exists(PACKAGE_BIN)

    again = _install(container, mock_nix_server, mirror_cache)

    assert again.returncode == 0, again.stderr
    output = (again.stdout + again.stderr).lower()
    assert f"installed: {INSTALL_TEST_PACKAGE}" in output
    assert "could not be recovered" not in output
    _assert_consistent(container)


def test_a_command_killed_after_the_switch_leaves_a_consistent_profile(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    container.exec(
        "bash", "-c", "printf '#!/bin/sh\\nexec sleep 300\\n' > /tmp/stalled-git && chmod +x /tmp/stalled-git",
        check=True,
    )

    _kill_during(
        container,
        mock_nix_server,
        mirror_cache,
        "/tmp/stalled-git",
        env={"MIX_GIT_PATH": "/tmp/stalled-git"},
    )
    assert container.path_exists(PACKAGE_BIN)

    again = _install(container, mock_nix_server, mirror_cache)

    assert again.returncode == 0, again.stderr
    assert "already installed" in (again.stdout + again.stderr).lower()
    assert _packages(_state(container)) == ["git", INSTALL_TEST_PACKAGE]
    assert _state(container) == _generation_state(container)


def test_a_broken_package_list_is_restored_from_the_active_profile(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    assert _install(container, mock_nix_server, mirror_cache).returncode == 0
    container.exec("bash", "-c", f"printf '{{broken' > {STATE}", user=USER, check=True)

    again = _install(container, mock_nix_server, mirror_cache)

    assert again.returncode == 0, again.stderr
    output = (again.stdout + again.stderr).lower()
    assert "already installed" in output
    assert "could not be recovered" not in output
    assert _packages(_state(container)) == ["git", INSTALL_TEST_PACKAGE]
    _assert_consistent(container)


def test_repair_restores_a_broken_package_list_without_dropping_packages(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    assert _install(container, mock_nix_server, mirror_cache).returncode == 0
    home_before = container.exec("cat", f"{STATE_DIR}/home.nix", check=True).stdout
    container.exec("bash", "-c", f"printf '{{broken' > {STATE}", user=USER, check=True)

    doctor = container.exec("sudo", "mix", "doctor", user=USER)
    assert doctor.returncode != 0
    assert STATE in doctor.stdout + doctor.stderr
    assert f"{STATE_DIR}/home.nix:" not in doctor.stdout + doctor.stderr

    repair = container.exec("sudo", "mix", "repair", user=USER)

    assert repair.returncode == 0, repair.stderr
    assert _packages(_state(container)) == ["git", INSTALL_TEST_PACKAGE]
    assert container.exec("cat", f"{STATE_DIR}/home.nix", check=True).stdout == home_before
    assert container.exec("sudo", "mix", "doctor", user=USER).returncode == 0


def test_a_change_that_cannot_be_recorded_in_git_still_takes_effect(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    container.exec("touch", f"{STATE_DIR}/.git/index.lock", user=USER, check=True)

    result = _install(container, mock_nix_server, mirror_cache)

    assert result.returncode == 0, result.stderr
    assert "record" not in (result.stdout + result.stderr).lower()
    assert _packages(_state(container)) == ["git", INSTALL_TEST_PACKAGE]
    _assert_consistent(container)


def test_remove_restores_a_broken_package_list_before_removing(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    assert _install(container, mock_nix_server, mirror_cache).returncode == 0
    container.exec("bash", "-c", f"printf '{{broken' > {STATE}", user=USER, check=True)

    result = container.exec(
        "mix",
        "remove",
        INSTALL_TEST_PACKAGE,
        *mirror_args(mock_nix_server, mirror_cache),
        user=USER,
    )

    assert result.returncode == 0, result.stderr
    assert f"removed: {INSTALL_TEST_PACKAGE}" in (result.stdout + result.stderr).lower()
    assert _packages(_state(container)) == ["git"]
    _assert_consistent(container)
