import pytest

from support.container import create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, mirror_args

USER = MIRROR_TEST_USERS[0]
DAEMON = "/usr/local/bin/mix-daemon"
MIX_MANAGED_MARKER = "/nix/.mix-managed"
UNUSABLE_PROXY = "http://127.0.0.1:9"
RUNTIME_DOWNLOAD = r"/nix-[^/]*\.tar"


def _restricted_sudo(container, user: str) -> None:
    create_user(container, user)
    container.exec(
        "bash",
        "-c",
        f"echo '{user} ALL=(ALL) NOPASSWD: {DAEMON}' > /etc/sudoers.d/{user} "
        f"&& chmod 440 /etc/sudoers.d/{user}",
        check=True,
    )


def test_bootstrap_works_when_sudo_only_allows_mix(container, mock_nix_server, mirror_cache):
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
    (runtime,) = [
        change for change in run.changes if change["operation"] == "OPERATION_INSTALL_RUNTIME"
    ]
    assert runtime["subject"].startswith(f"{url}/"), run


def test_repair_works_when_sudo_only_allows_mix(container, mock_nix_server, mirror_cache):
    _restricted_sudo(container, USER)
    assert container.mix(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    ).succeeded()
    container.exec("groupmod", "--gid", "9999", "nixbld", check=True)

    run = container.mix("repair", user=USER)

    assert run.succeeded(), run
    assert {"target": "nixbld", "fixed": True} in run.result("repair")["reports"], run
    assert container.exec("getent", "group", "nixbld").stdout.split(":")[2] == "30000"


def test_a_user_without_sudo_rights_cannot_bootstrap(container):
    create_user(container, "nosudo")

    run = container.mix("bootstrap", user="nosudo")

    assert run.exit_code == 1, run
    assert run.code == "CODE_PRIVILEGES_UNAVAILABLE", run


@pytest.mark.bootstrapped
def test_the_daemon_serves_no_user_outside_mix_users(container):
    create_user(container, "outsider")

    run = container.mix("repair", user="outsider")

    assert run.exit_code == 1, run
    assert run.code == "CODE_NOT_BOOTSTRAPPED", run


@pytest.mark.bootstrapped
def test_an_enrolled_user_installs_through_the_daemon_without_sudo(container):
    container.exec("rm", f"/etc/sudoers.d/{USER}", check=True)
    assert container.exec("sudo", "-n", "true", user=USER).returncode != 0

    run = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)

    assert run.succeeded(), run
    assert run.result("install")["added"] == [INSTALL_TEST_PACKAGE], run


def test_ctrl_c_through_the_worker_rolls_back_and_says_so(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    gate = container.gate(RUNTIME_DOWNLOAD)
    bootstrap = container.mix_background(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    gate.reached(unless=bootstrap)
    client = bootstrap.pid()
    launcher = container.exec(
        "pgrep", "-f", f"^sudo {DAEMON} serve-stdin", check=True
    ).stdout.split()[0]
    container.exec("kill", "-INT", client, launcher, check=True)

    run = bootstrap.wait()

    assert run.status == "STATUS_CANCELLED", run
    assert run.cancellation == "CANCELLATION_INTERRUPTED", run
    assert {"create-users-and-groups", "create-nix-dir"} <= set(run.rolled_back()), run
    assert not container.path_exists(MIX_MANAGED_MARKER)
    assert container.exec("getent", "group", "nixbld").returncode != 0


def _wait_until_gone(container, pid: str) -> None:
    container.exec("tail", f"--pid={pid}", "-f", "/dev/null", check=True)


def test_a_killed_client_still_gets_its_changes_rolled_back(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    gate = container.gate(RUNTIME_DOWNLOAD)
    bootstrap = container.mix_background(
        "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    gate.reached(unless=bootstrap)
    worker = container.exec(
        "pgrep", "-f", f"^sudo {DAEMON} serve-stdin", check=True
    ).stdout.split()[0]

    container.exec("kill", "-KILL", bootstrap.pid(), check=True)
    _wait_until_gone(container, worker)

    assert not container.path_exists(MIX_MANAGED_MARKER)
    assert container.exec("getent", "group", "nixbld").returncode != 0


@pytest.mark.bootstrapped
def test_json_output_is_one_document_on_stdout_even_when_verbose(
    container, mock_nix_server, mirror_cache
):
    container.exec("rm", "/etc/profile.d/mix-nix.sh", check=True)

    run = container.mix("-vv", "repair", user=USER)

    assert run.succeeded(), run
    assert run.document["formatVersion"] == "1.0", run
    assert {"target": "/etc/profile.d/mix-nix.sh", "fixed": True} in run.result("repair")["reports"]
