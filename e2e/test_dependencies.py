import json

import pytest

from support.container import until
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]
HOME = f"/home/{USER}"
STATE_DIR = f"{HOME}/.local/state/mix"
PROFILES = f"{HOME}/.local/state/nix/profiles"
DAEMON_BIN = "/var/lib/mix/bin/mix-daemon"
JOURNALS = "/var/lib/mix/journal"
LEFTOVERS = "leftovers of interrupted writes"


def _findings(run) -> dict[str, dict]:
    return {
        report["target"]: report["finding"]
        for report in run.result("doctor").get("reports", [])
        if "finding" in report
    }


def _healthy(container) -> None:
    run = container.mix("doctor", user=USER)
    assert run.exit_code == 0, _findings(run)


@pytest.mark.bootstrapped
def test_a_users_own_nix_settings_never_change_what_mix_builds(
    container, mock_nix_server, mirror_cache
):
    settings = "experimental-features =\\nsubstituters = http://127.0.0.1:9\\n"
    container.exec(
        "bash",
        "-c",
        f"mkdir -p {HOME}/.config/nix && printf '{settings}' > {HOME}/.config/nix/nix.conf",
        user=USER,
        check=True,
    )

    installed = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)

    assert installed.succeeded(), installed


@pytest.mark.bootstrapped
def test_a_profile_lock_someone_else_holds_is_waited_for_in_the_open(
    container, mock_nix_server, mirror_cache
):
    lock = f"{PROFILES}/profile.lock"
    holder = container.start_background("flock", "-x", lock, "sleep", "infinity", user=USER)
    until(
        lambda: container.exec("pgrep", "-u", USER, "-x", "sleep").returncode == 0,
        "the profile lock being held",
        unless=holder,
    )
    inode = container.exec("stat", "-c", "%i", lock, check=True).stdout.strip()
    install = container.mix_background("install", INSTALL_TEST_PACKAGE, user=USER)

    def waiting() -> bool:
        table = container.exec("cat", "/proc/locks", check=True).stdout
        return any(
            "->" in line and line.split()[-3].endswith(f":{inode}") for line in table.splitlines()
        )

    until(waiting, "the install waiting for the profile lock", unless=install)
    container.exec("pkill", "-u", USER, "-x", "sleep", check=True)
    run = install.wait()

    assert run.succeeded(), run
    (waited,) = run.waits
    assert waited["lock"] == lock
    assert waited["holder"] == USER
    assert "flock" in waited["command"]


@pytest.mark.bootstrapped
def test_an_interrupted_request_repair_cannot_put_back_is_given_up(container):
    records = [
        {"began": {"request": "r9"}},
        {
            "prepared": {
                "seq": 0,
                "undo": [
                    {
                        "Restore": {
                            "path": "/etc/mix/policy.json",
                            "from": "/etc/mix/.policy.json.mix-backup-r9-1",
                            "expect": "Absent",
                        }
                    }
                ],
            }
        },
        {"done": {"seq": 0}},
    ]
    journal = "\n".join(json.dumps(record) for record in records) + "\n"
    container.exec("bash", "-c", f"cat > {JOURNALS}/r9.ndjson <<'EOF'\n{journal}EOF", check=True)
    container.exec("systemctl", "restart", "mix-daemon.service", check=True)

    found = container.mix("doctor", user=USER)
    assert found.exit_code == 3, found
    assert _findings(found) == {
        JOURNALS: {"interrupted": {"requests": ["r9"], "pending": ["/etc/mix/policy.json"]}}
    }
    repaired = container.mix("repair", user=USER)
    assert repaired.succeeded(), repaired
    assert repaired.warnings == ["CODE_CLEANUP_INCOMPLETE"], repaired
    assert not container.path_exists(f"{JOURNALS}/r9.ndjson")
    _healthy(container)


@pytest.mark.bootstrapped
def test_what_an_interrupted_write_left_is_reported_and_removed(container):
    leftover = f"{STATE_DIR}/.state.mix-backup-r9-1"
    container.exec("bash", "-c", f"echo '{{}}' > {leftover}", user=USER, check=True)

    found = container.mix("doctor", user=USER)
    assert found.exit_code == 3, found
    assert _findings(found) == {LEFTOVERS: {"leftovers": {"paths": [leftover]}}}

    assert container.mix("repair", user=USER).succeeded()
    assert not container.path_exists(leftover)
    _healthy(container)


@pytest.mark.bootstrapped
def test_an_altered_daemon_binary_is_replaced_by_the_running_one(container):
    container.exec(
        "bash",
        "-c",
        f"cp {DAEMON_BIN} /tmp/altered && echo tampered >> /tmp/altered"
        f" && mv /tmp/altered {DAEMON_BIN}",
        check=True,
    )

    found = container.mix("doctor", user=USER)
    assert found.exit_code == 3, found
    assert _findings(found) == {DAEMON_BIN: {"contentDrift": {}}}

    assert container.mix("repair", user=USER).succeeded()
    pid = container.exec(
        "systemctl", "show", "-P", "MainPID", "mix-daemon.service", check=True
    ).stdout.strip()
    same = container.exec("cmp", DAEMON_BIN, f"/proc/{pid}/exe")
    assert same.returncode == 0, same.stdout
    _healthy(container)


@pytest.mark.bootstrapped
def test_a_generation_whose_store_path_is_gone_is_deleted(container):
    dangling = f"{PROFILES}/home-manager-9-link"
    container.exec(
        "ln",
        "-s",
        "/nix/store/00000000000000000000000000000000-gone",
        dangling,
        user=USER,
        check=True,
    )

    found = container.mix("doctor", user=USER)
    assert found.exit_code == 3, found
    assert _findings(found) == {
        f"{PROFILES}/home-manager": {"generationDangling": {"generation": "9"}}
    }

    assert container.mix("repair", user=USER).succeeded()
    assert container.exec("test", "-L", dangling).returncode != 0
    _healthy(container)


@pytest.mark.bootstrapped
def test_a_file_in_the_way_of_a_managed_one_is_reported_and_left_alone(container):
    generation = container.exec(
        "readlink", "-f", f"{PROFILES}/home-manager", check=True
    ).stdout.strip()
    managed = container.exec(
        "bash",
        "-c",
        f"cd {generation}/home-files && find . -mindepth 1 \\( -type l -o -type f \\) | head -1",
        check=True,
    ).stdout.strip()
    assert managed, "the active generation links no file into the home"
    target = f"{HOME}/{managed.removeprefix('./')}"
    container.exec("bash", "-c", f"rm -f {target} && echo mine > {target}", user=USER, check=True)

    found = container.mix("doctor", user=USER)
    assert found.exit_code == 3, found
    assert _findings(found) == {HOME: {"inTheWay": {"paths": [target]}}}
    assert container.mix("repair", user=USER).exit_code == 3
    assert container.exec("cat", target, check=True).stdout == "mine\n"
