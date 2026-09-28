import time

from support.mirror import silent_mirror

MIX_MANAGED_MARKER = "/nix/.mix-managed"
PROVISIONING_MANIFEST = "/nix/.mix-provisioning-manifest"
DEFAULT_PROFILE_NIX_ENV = "/nix/var/nix/profiles/default/bin/nix-env"
RUNNING_CREATE_USERS_AND_GROUPS = "running: create the managed groups and build users"
RUNNING_FETCH_AND_UNPACK = "running: fetch and activate the managed runtime"
WINDING_DOWN_NOTICE = "Cancelling... (cleaning up)"


def _nixbld_users(container) -> list[str]:
    result = container.exec(
        "sh", "-c", 'for i in $(seq 1 32); do id "nixbld$i" >/dev/null 2>&1 && echo "nixbld$i"; done'
    )
    return [line for line in result.stdout.splitlines() if line]


def _store_entry_count(container) -> int:
    result = container.exec("sh", "-c", "ls /nix/store 2>/dev/null | wc -l")
    return int(result.stdout.strip() or "0")


def test_sigint_during_a_fast_step_exits_promptly_and_rolls_back_cleanly(container, mock_nix_server):
    proc = container.start_background(
        "mix", "-v", "bootstrap", env={"MIX_NIX_MIRROR": mock_nix_server["url"]}
    )
    proc.wait_for_output(RUNNING_CREATE_USERS_AND_GROUPS)
    proc.signal("INT")

    result = proc.wait(timeout=15.0)

    assert result.returncode != 0, result.stdout
    assert WINDING_DOWN_NOTICE in result.stdout
    assert not container.path_exists(MIX_MANAGED_MARKER)
    assert container.exec("getent", "group", "nixbld").returncode != 0
    assert container.exec("getent", "group", "mix-users").returncode != 0
    assert _nixbld_users(container) == []


def test_sigint_during_fetch_and_unpack_exits_promptly_and_rolls_back(container):
    with silent_mirror() as silent:
        proc = container.start_background("mix", "bootstrap", env={"MIX_NIX_MIRROR": silent})
        proc.wait_for_output(RUNNING_FETCH_AND_UNPACK)
        proc.signal("INT")

        started = time.time()
        result = proc.wait(timeout=15.0)
        elapsed = time.time() - started

    assert result.returncode != 0, result.stdout
    assert elapsed < 10.0
    assert WINDING_DOWN_NOTICE in result.stdout
    assert not container.path_exists(PROVISIONING_MANIFEST)
    assert _store_entry_count(container) == 0
    assert not container.path_exists(DEFAULT_PROFILE_NIX_ENV)


def test_hard_kill_during_fetch_and_unpack_then_bootstrap_converges(container, mock_nix_server):
    with silent_mirror() as silent:
        proc = container.start_background("mix", "bootstrap", env={"MIX_NIX_MIRROR": silent})
        proc.wait_for_output(RUNNING_FETCH_AND_UNPACK)
        proc.signal("KILL")

        result = proc.wait(timeout=15.0)
    assert result.returncode == 137

    fix = container.exec(
        "mix", "bootstrap", env={"MIX_NIX_MIRROR": mock_nix_server["url"]}
    )
    assert fix.returncode == 0, fix.stderr

    assert container.path_exists(DEFAULT_PROFILE_NIX_ENV)
    assert not container.path_exists(PROVISIONING_MANIFEST)
    check = container.exec("mix", "doctor")
    assert check.returncode == 0, check.stderr


def test_a_store_left_half_rolled_back_is_provisioned_again(container, mock_nix_server):
    env = {"MIX_NIX_MIRROR": mock_nix_server["url"]}
    first = container.exec("mix", "bootstrap", env=env)
    assert first.returncode == 0, first.stdout + first.stderr
    container.exec(
        "sh",
        "-c",
        "rm -f /nix/var/nix/profiles/default /nix/var/nix/profiles/default-*-link"
        " && rm -rf /nix/store/*-user-environment /nix/store/*-user-environment.drv",
        check=True,
    )

    again = container.exec("mix", "-v", "--no-progress", "bootstrap", env=env)

    assert again.returncode == 0, again.stdout + again.stderr
    assert container.path_exists(DEFAULT_PROFILE_NIX_ENV)
