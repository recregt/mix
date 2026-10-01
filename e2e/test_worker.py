import time

import pytest
from support.container import create_user
from support.mirror import MIRROR_TEST_USERS, mirror_args

USER = MIRROR_TEST_USERS[0]
DAEMON = "/usr/local/bin/mix-daemon"
MIX_MANAGED_MARKER = "/nix/.mix-managed"
UNUSABLE_PROXY = "http://127.0.0.1:9"


def _restricted_sudo(container, user: str) -> None:
    create_user(container, user)
    container.exec(
        "bash",
        "-c",
        f"echo '{user} ALL=(ALL) NOPASSWD: {DAEMON}' > /etc/sudoers.d/{user} "
        f"&& chmod 440 /etc/sudoers.d/{user}",
        check=True,
    )


def test_bootstrap_works_when_sudo_only_allows_mix(
    container, mock_nix_server, mirror_cache
):
    _restricted_sudo(container, USER)
    _, url, _, key = mirror_args(mock_nix_server, mirror_cache)

    run = container.mix(
        "bootstrap",
        env={
            "MIX_NIX_MIRROR": url,
            "MIX_NIX_MIRROR_KEY": key,
            "HTTP_PROXY": UNUSABLE_PROXY,
            "HTTPS_PROXY": UNUSABLE_PROXY,
        },
        user=USER,
    )

    assert run.succeeded(), run
    assert [fetch["url"].startswith(f"{url}/") for fetch in run.progress("fetch")] == [
        True
    ], run


def test_repair_works_when_sudo_only_allows_mix(
    container, mock_nix_server, mirror_cache
):
    _restricted_sudo(container, USER)
    assert container.mix(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    ).succeeded()
    container.exec("groupmod", "--gid", "9999", "nixbld", check=True)

    run = container.mix("repair", user=USER)

    assert run.succeeded(), run
    assert {"target": "nixbld", "fixed": True} in run.result("repair")["reports"], run
    assert container.exec("getent", "group", "nixbld").stdout.split(":")[2] == "30000"


def test_a_user_without_sudo_rights_is_told_so(container):
    create_user(container, "nosudo")

    run = container.mix("repair", user="nosudo")

    assert run.exit_code == 1, run
    assert run.code == "CODE_PRIVILEGES_UNAVAILABLE", run


def test_ctrl_c_through_the_worker_rolls_back_and_says_so(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    bootstrap = container.mix_background(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    bootstrap.wait_for_step("create-users-and-groups")
    client = bootstrap.pid()
    launcher = container.exec(
        "pgrep", "-f", f"^sudo {DAEMON} serve-stdin", check=True
    ).stdout.split()[0]
    container.exec("kill", "-INT", client, launcher, check=True)

    run = bootstrap.wait()

    assert run.status == "STATUS_CANCELLED", run
    assert run.cancellation == "CANCELLATION_INTERRUPTED", run
    assert run.progress("stopping"), run
    assert {"create-users-and-groups", "create-nix-dir"} <= set(run.rolled_back()), run
    assert not container.path_exists(MIX_MANAGED_MARKER)
    assert container.exec("getent", "group", "nixbld").returncode != 0


def _wait_until_gone(container, pattern: str, timeout: float) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if container.exec("pgrep", "-f", pattern).returncode != 0:
            return
        time.sleep(0.2)
    raise TimeoutError(f"a process matching {pattern!r} was still running")


def test_a_killed_client_still_gets_its_changes_rolled_back(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    bootstrap = container.mix_background(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    bootstrap.wait_for_step("create-users-and-groups")
    worker = f"^sudo {DAEMON} serve-stdin"
    container.exec("pgrep", "-f", worker, check=True)

    container.exec("kill", "-KILL", bootstrap.pid(), check=True)
    _wait_until_gone(container, worker, timeout=60)

    assert not container.path_exists(MIX_MANAGED_MARKER)
    assert container.exec("getent", "group", "nixbld").returncode != 0


@pytest.mark.bootstrapped
def test_a_command_run_through_the_worker_streams_json_and_nothing_else(
    container, mock_nix_server, mirror_cache
):
    container.exec("rm", "/etc/profile.d/mix-nix.sh", check=True)

    run = container.mix("--output", "json", "-vv", "repair", user=USER)

    assert run.succeeded(), run
    assert run.stderr == ""
    assert {"target": "/etc/profile.d/mix-nix.sh", "fixed": True} in run.result(
        "repair"
    )["reports"]
