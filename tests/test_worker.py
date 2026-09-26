from conftest import MIRROR_TEST_USERS, create_user, mirror_args

USER = MIRROR_TEST_USERS[0]
MIX = "/usr/local/bin/mix"
MIX_MANAGED_MARKER = "/nix/.mix-managed"
RUNNING_CREATE_USERS_AND_GROUPS = "running: create the managed groups and build users"
WINDING_DOWN_NOTICE = "Cancelling... (cleaning up)"
UNUSABLE_PROXY = "http://127.0.0.1:9"


def _restricted_sudo(container, user: str) -> None:
    create_user(container, user)
    container.exec(
        "bash",
        "-c",
        f"echo '{user} ALL=(ALL) NOPASSWD: {MIX}' > /etc/sudoers.d/{user} "
        f"&& chmod 440 /etc/sudoers.d/{user}",
        check=True,
    )


def test_bootstrap_works_when_sudo_only_allows_mix(container, mock_nix_server, mirror_cache):
    _restricted_sudo(container, USER)
    _, url, _, key = mirror_args(mock_nix_server, mirror_cache)

    result = container.exec(
        "mix",
        "-v",
        "--no-progress",
        "bootstrap",
        env={
            "MIX_NIX_MIRROR": url,
            "MIX_NIX_MIRROR_KEY": key,
            "HTTP_PROXY": UNUSABLE_PROXY,
            "HTTPS_PROXY": UNUSABLE_PROXY,
        },
        user=USER,
    )

    output = result.stdout + result.stderr
    assert result.returncode == 0, output
    assert "not allowed to set the following environment variables" not in output
    assert f"fetching runtime archive: {url}/" in output
    assert "mix is ready!" in output


def test_repair_works_when_sudo_only_allows_mix(container, mock_nix_server, mirror_cache):
    _restricted_sudo(container, USER)
    mirror = mirror_args(mock_nix_server, mirror_cache)
    assert container.exec("mix", "bootstrap", *mirror, user=USER).returncode == 0
    container.exec("groupmod", "--gid", "9999", "nixbld", check=True)

    result = container.exec("mix", "repair", user=USER)

    assert result.returncode == 0, result.stdout + result.stderr
    assert container.exec("getent", "group", "nixbld").stdout.split(":")[2] == "30000"


def test_a_user_without_sudo_rights_is_told_so(container):
    create_user(container, "nosudo")

    result = container.exec("mix", "repair", user="nosudo")

    assert result.returncode != 0
    output = (result.stdout + result.stderr).lower()
    assert "couldn't get administrator rights" in output
    assert "sudo" in output


def test_ctrl_c_through_the_worker_rolls_back_and_says_so(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    proc = container.start_background(
        "mix", "-v", "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    proc.wait_for_output(RUNNING_CREATE_USERS_AND_GROUPS, timeout=60)
    client = proc.pid()
    launcher = container.exec("pgrep", "-f", f"^sudo {MIX} worker", check=True).stdout.split()[0]
    container.exec("kill", "-INT", client, launcher, check=True)

    result = proc.wait(timeout=60)

    assert result.returncode != 0, result.stdout
    assert WINDING_DOWN_NOTICE in result.stdout
    assert "rolling back: create the managed groups and build users" in result.stdout
    assert "rolling back: create /nix" in result.stdout
    assert "stopped; everything it had changed was undone" in result.stdout.lower()
    assert not container.path_exists(MIX_MANAGED_MARKER)
    assert container.exec("getent", "group", "nixbld").returncode != 0
