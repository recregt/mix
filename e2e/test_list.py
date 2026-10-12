import pytest

from support.container import create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]


def _packages(run):
    return run.result("list").get("packages", [])


def _names(run):
    return [package["name"] for package in _packages(run)]


@pytest.mark.bootstrapped
def test_list_names_what_was_installed_and_none_of_home_managers_own_packages(
    container, mock_nix_server, mirror_cache
):
    run = container.mix("list", user=USER)

    assert run.succeeded(), run
    assert _names(run) == ["git"], run.document
    git = _packages(run)[0]
    output = git["outputs"][0]
    assert git["pname"] == "git"
    assert git["version"]
    assert git["description"]
    assert git["homepage"]
    assert git["licenses"]
    assert output["output"] == "out"
    assert output["storePath"].endswith(f"-git-{git['version']}")
    assert int(output["narSize"]) > 0
    assert output["narHash"].startswith("sha256-")
    active = run.result("list")["active"]
    assert active["storePath"].endswith("-home-manager-generation")
    assert int(active["number"]) > 0


@pytest.mark.bootstrapped
def test_list_follows_an_install_and_a_remove(container, mock_nix_server, mirror_cache):
    before = container.mix("list", user=USER)
    assert before.succeeded(), before

    installed = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert installed.succeeded(), installed
    after = container.mix("list", user=USER)

    assert after.succeeded(), after
    assert _names(after) == sorted(["git", INSTALL_TEST_PACKAGE]), after.document
    moved, was = after.result("list")["active"], before.result("list")["active"]
    assert int(moved["number"]) > int(was["number"])
    assert moved["storePath"] != was["storePath"]
    added = next(package for package in _packages(after) if package["name"] == INSTALL_TEST_PACKAGE)
    assert added["version"]
    assert added["outputs"][0]["storePath"].endswith(f"-{INSTALL_TEST_PACKAGE}-{added['version']}")

    removed = container.mix("remove", INSTALL_TEST_PACKAGE, user=USER)
    assert removed.succeeded(), removed
    last = container.mix("list", user=USER)

    assert _names(last) == ["git"], last.document


@pytest.mark.bootstrapped
def test_list_serves_no_user_outside_mix_users(container):
    create_user(container, "outsider")

    run = container.mix("list", user="outsider")

    assert run.exit_code == 1, run
    assert run.code == "CODE_NOT_BOOTSTRAPPED", run
