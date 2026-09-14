import ast
import pathlib
import re

MIX_MANAGED_MARKER = "/nix/.mix-managed"
DEFAULT_PROFILE_BIN = "/nix/var/nix/profiles/default/bin"
SYSTEM_PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"


def _nix_conf_content() -> str:
    models_src = (
        pathlib.Path(__file__).resolve().parents[2] / "crates/core/src/models.rs"
    ).read_text()
    match = re.search(r'NIX_CONF:\s*&str\s*=\s*(".*?");', models_src, re.S)
    if not match:
        raise RuntimeError("could not find the NIX_CONF constant in models.rs")
    return ast.literal_eval(match.group(1))


NIX_CONF_CONTENT = _nix_conf_content()


def _bootstrap(container, mock_nix_server):
    result = container.exec(
        "mix",
        "bootstrap",
        env={"MIX_NIX_MIRROR": mock_nix_server["url"]},
    )
    assert result.returncode == 0, result.stderr


def test_bootstrap_accepts_mirror_as_a_cli_flag(container, mock_nix_server):
    result = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stderr
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_bootstrap_is_silent_by_default(container, mock_nix_server):
    result = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stderr
    assert "running command" not in result.stderr.lower()


def test_bootstrap_verbose_shows_command_execution(container, mock_nix_server):
    result = container.exec(
        "mix", "-vv", "bootstrap", "--mirror", mock_nix_server["url"]
    )

    assert result.returncode == 0, result.stderr
    assert "running command" in result.stderr.lower()


def test_bootstrap_mirror_flag_takes_precedence_over_the_env_var(container, mock_nix_server):
    result = container.exec(
        "mix",
        "bootstrap",
        "--mirror",
        mock_nix_server["url"],
        env={"MIX_NIX_MIRROR": "http://invalid.internal"},
    )

    assert result.returncode == 0, result.stderr
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_bootstrap_fails_cleanly_with_an_unreachable_mirror(container):
    result = container.exec("mix", "bootstrap", "--mirror", "http://127.0.0.1:1")

    assert result.returncode != 0
    assert "network" in (result.stdout + result.stderr).lower()
    assert not container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_repair_succeeds_with_a_clean_shell_path(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    repair = container.exec("mix", "repair", env={"PATH": SYSTEM_PATH})

    assert repair.returncode == 0, repair.stderr
    assert container.path_exists(MIX_MANAGED_MARKER)
    assert container.exec("getent", "group", "nixbld").returncode == 0


def test_repair_succeeds_when_path_still_points_into_the_deleted_snippet(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    polluted_path = f"{DEFAULT_PROFILE_BIN}:{SYSTEM_PATH}"
    repair = container.exec("mix", "repair", env={"PATH": polluted_path})

    assert repair.returncode == 0, repair.stderr
    assert container.exec("getent", "group", "nixbld").returncode == 0


def test_doctor_detects_a_stopped_socket_alone(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("systemctl", "stop", "nix-daemon.socket")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a stopped socket with nothing else corrupted"


def test_doctor_check_succeeds_for_a_non_root_user(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("useradd", "--create-home", "plainuser", check=True)

    check = container.exec("mix", "doctor", user="plainuser")
    assert check.returncode == 0, check.stderr + check.stdout


def test_repair_fixes_a_nix_dir_permission_drift(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("chmod", "700", "/nix")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect /nix permission drift"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("stat", "-c", "%a", "/nix").stdout.strip() == "755"


def test_repair_fixes_a_deleted_profile_snippet(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("rm", "-f", "/etc/profile.d/mix-nix.sh")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a deleted profile snippet"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.path_exists("/etc/profile.d/mix-nix.sh")


def test_repair_fixes_a_wrong_nixbld_gid(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("groupmod", "--gid", "9999", "nixbld")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a nixbld group with the wrong gid"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("getent", "group", "nixbld").stdout.split(":")[2] == "30000"


def test_repair_fixes_a_wrong_nixbld_user_gid(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("usermod", "--gid", "0", "nixbld1", check=True)

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a nixbld1 user with the wrong gid"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("id", "-g", "nixbld1").stdout.strip() == "30000"


def test_repair_cannot_restore_a_deleted_default_profile_but_bootstrap_can(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("rm", "-rf", "/nix/var/nix/profiles/default")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a missing default profile"

    repair = container.exec("mix", "repair")
    assert repair.returncode != 0, "repair cannot reinstall the Nix runtime itself"
    assert "mix bootstrap" in (repair.stdout + repair.stderr).lower()
    assert not container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")

    fix = container.exec("mix", "bootstrap", "--mirror", mock_nix_server["url"])
    assert fix.returncode == 0, fix.stderr

    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")
    assert container.exec("mix", "doctor").returncode == 0


def test_bootstrap_auto_escalates_for_a_sudo_user(container, mock_nix_server):
    container.exec("useradd", "--create-home", "ciuser", check=True)
    container.exec(
        "bash",
        "-c",
        "echo 'ciuser ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/ciuser",
        check=True,
    )

    result = container.exec(
        "mix", "bootstrap", "--mirror", mock_nix_server["url"], user="ciuser"
    )

    assert result.returncode == 0, result.stderr
    assert "re-running with sudo" in result.stderr.lower()
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")


def test_repair_fixes_injected_drift(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("bash", "-c", "echo corrupted > /etc/nix/nix.conf")
    container.exec("userdel", "nixbld1")
    container.exec("systemctl", "stop", "nix-daemon.socket")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect the injected drift before any repair"

    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("cat", "/etc/nix/nix.conf").stdout == NIX_CONF_CONTENT
    assert container.exec("getent", "passwd", "nixbld1").returncode == 0
    assert container.exec("systemctl", "is-active", "nix-daemon.socket").stdout.strip() == "active"


def test_repair_fixes_independent_targets_even_when_one_target_fails(container, mock_nix_server):
    _bootstrap(container, mock_nix_server)

    container.exec("chmod", "700", "/nix/var/nix/userpool")
    container.exec("userdel", "nixbld1")
    container.exec("bash", "-c", "mv /usr/sbin/useradd /usr/sbin/useradd.disabled")

    repair = container.exec("mix", "repair")
    assert repair.returncode != 0, "expected the disabled useradd to leave nixbld1 unrepaired"

    assert container.exec("stat", "-c", "%a", "/nix/var/nix/userpool").stdout.strip() == "755", (
        "the userpool directory fix is independent of the failed user fix and should still apply"
    )
    assert container.path_exists("/nix/var/nix/profiles/default/bin/nix-env")

    container.exec("bash", "-c", "mv /usr/sbin/useradd.disabled /usr/sbin/useradd")
    repair = container.exec("mix", "repair")
    assert repair.returncode == 0, repair.stderr

    assert container.exec("getent", "passwd", "nixbld1").returncode == 0
