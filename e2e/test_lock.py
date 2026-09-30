import shlex

from support.container import create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, mirror_args

USER = MIRROR_TEST_USERS[0]
PACKAGE_BIN = f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}"


def test_install_works_in_the_same_session_as_bootstrap(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)
    mirror = shlex.join(mirror_args(mock_nix_server, mirror_cache))
    bootstrap, install = container.capture(), container.capture()

    result = container.exec(
        "bash",
        "-c",
        f"mix --events-file {bootstrap} bootstrap {mirror}"
        f" && mix --events-file {install} install {INSTALL_TEST_PACKAGE}",
        user=USER,
    )

    assert container.recorded(bootstrap, 0, "", result.stderr).succeeded()
    installed = container.recorded(install, result.returncode, "", result.stderr)
    assert installed.succeeded(), installed
    assert installed.result("install")["added"] == [INSTALL_TEST_PACKAGE]
    assert container.path_exists(PACKAGE_BIN)
