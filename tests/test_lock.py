import shlex

from conftest import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, create_user, mirror_args
from test_install import _bootstrapped

USER = MIRROR_TEST_USERS[0]
LOCK = "/var/lib/mix/lock"
PACKAGE_BIN = f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"


def test_install_works_in_the_same_session_as_bootstrap(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)
    mirror = shlex.join(mirror_args(mock_nix_server, mirror_cache))

    result = container.exec(
        "bash",
        "-c",
        f"mix bootstrap {mirror} && mix install {INSTALL_TEST_PACKAGE} {mirror}",
        user=USER,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert container.path_exists(PACKAGE_BIN)


def test_the_lock_is_readable_by_everyone_and_owned_by_root(container, mock_nix_server, mirror_cache):
    _bootstrapped(container, mock_nix_server, mirror_cache)

    stat = container.exec("stat", "-c", "%a %U", LOCK, check=True).stdout.split()

    assert stat == ["644", "root"]


def test_a_missing_lock_is_explained_and_bootstrap_puts_it_back(
    container, mock_nix_server, mirror_cache
):
    _bootstrapped(container, mock_nix_server, mirror_cache)
    container.exec("rm", LOCK, check=True)

    refused = container.exec(
        "mix", "install", INSTALL_TEST_PACKAGE, *mirror_args(mock_nix_server, mirror_cache), user=USER
    )

    assert refused.returncode != 0
    output = (refused.stdout + refused.stderr).lower()
    assert "isn't set up yet" in output
    assert "mix bootstrap" in output
    assert "permission" not in output

    again = container.exec("mix", "bootstrap", *mirror_args(mock_nix_server, mirror_cache), user=USER)
    assert again.returncode == 0, again.stderr
    installed = container.exec(
        "mix", "install", INSTALL_TEST_PACKAGE, *mirror_args(mock_nix_server, mirror_cache), user=USER
    )
    assert installed.returncode == 0, installed.stderr
    assert container.path_exists(PACKAGE_BIN)
