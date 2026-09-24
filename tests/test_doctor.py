from conftest import MIX_USERS_GROUP, bootstrap_root, create_user, daemon_trusts, group_members


def test_doctor_detects_a_stopped_socket_alone(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    container.exec("systemctl", "stop", "nix-daemon.socket")

    check = container.exec("mix", "doctor")
    assert check.returncode != 0, "doctor should detect a stopped socket with nothing else corrupted"


def test_doctor_ignores_a_user_mix_never_configured(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)

    create_user(container, "plainuser")

    check = container.exec("mix", "doctor", user="plainuser")
    assert check.returncode == 0, check.stderr + check.stdout
    assert not container.path_exists("/home/plainuser/.local/state/mix")
    assert group_members(container, MIX_USERS_GROUP) == []
    assert not daemon_trusts(container, "plainuser")
