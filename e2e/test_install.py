import pytest

from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]


@pytest.mark.bootstrapped
def test_install_adds_a_package_as_a_regular_user_with_no_sudo(
    container, mock_nix_server, mirror_cache
):
    state_dir = f"/home/{USER}/.local/state/mix"

    run = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)

    assert run.succeeded(), run
    assert run.result("install")["added"] == [INSTALL_TEST_PACKAGE]
    assert container.path_exists(f"/home/{USER}/.nix-profile/bin/{INSTALL_TEST_PACKAGE}")

    state = container.exec("cat", f"{state_dir}/state", check=True).stdout
    assert INSTALL_TEST_PACKAGE in state
    assert "git" in state

    home_nix = container.exec("cat", f"{state_dir}/home.nix", check=True).stdout
    assert INSTALL_TEST_PACKAGE in home_nix

    git_bin = f"/home/{USER}/.nix-profile/bin/git"
    log = container.exec(
        git_bin, "-C", state_dir, "log", "--format=%an <%ae>", user=USER, check=True
    )
    assert log.stdout.strip().splitlines()[0] == "mix <mix@localhost>"
    status = container.exec(git_bin, "-C", state_dir, "status", "--short", user=USER, check=True)
    assert status.stdout.strip() == ""

    again = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert again.succeeded(), again
    assert again.result("install") == {
        "added": [],
        "skipped": [INSTALL_TEST_PACKAGE],
        "restored": False,
        "checked": [],
    }
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == state


@pytest.mark.bootstrapped
def test_an_unknown_package_is_named_and_nothing_changes(container, mock_nix_server, mirror_cache):
    state_dir = f"/home/{USER}/.local/state/mix"
    before = container.exec("cat", f"{state_dir}/state", check=True).stdout

    run = container.mix("install", "ripgrep2", user=USER)

    assert run.exit_code == 1, run
    assert run.code == "CODE_UNKNOWN_PACKAGE", run
    assert run.document["problems"][0]["packages"]["packages"] == ["ripgrep2"], run
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == before


@pytest.mark.bootstrapped
def test_a_dry_run_names_an_unknown_package_as_the_install_would(
    container, mock_nix_server, mirror_cache
):
    state_dir = f"/home/{USER}/.local/state/mix"
    before = container.exec("cat", f"{state_dir}/state", check=True).stdout

    run = container.mix("install", "--dry-run", "ripgrep2", user=USER)

    assert run.exit_code == 1, run
    assert run.code == "CODE_UNKNOWN_PACKAGE", run
    assert run.document["problems"][0]["packages"]["packages"] == ["ripgrep2"], run
    assert run.result("install")["checked"] == [
        {"name": "ripgrep2", "verdict": "PACKAGE_VERDICT_UNKNOWN", "licenses": [], "outputs": []}
    ], run
    assert container.exec("cat", f"{state_dir}/state", check=True).stdout == before


@pytest.mark.bootstrapped
def test_a_refused_dry_run_still_plans_the_packages_that_would_install(
    container, mock_nix_server, mirror_cache
):
    run = container.mix("install", "--dry-run", "ripgrep2", INSTALL_TEST_PACKAGE, user=USER)

    assert run.code == "CODE_UNKNOWN_PACKAGE", run
    result = run.result("install")
    verdicts = {checked["name"]: checked["verdict"] for checked in result["checked"]}
    assert verdicts == {
        "ripgrep2": "PACKAGE_VERDICT_UNKNOWN",
        INSTALL_TEST_PACKAGE: "PACKAGE_VERDICT_AVAILABLE",
    }, result
    (available,) = [checked for checked in result["checked"] if checked["outputs"]]
    assert available["outputs"][0]["substituter"].startswith(mock_nix_server["url"]), available
    assert result["plan"]["complete"], result


@pytest.mark.bootstrapped
def test_a_dry_run_describes_each_new_package_and_what_it_would_fetch(
    container, mock_nix_server, mirror_cache
):
    run = container.mix("install", "--dry-run", INSTALL_TEST_PACKAGE, user=USER)

    assert run.succeeded(), run
    result = run.result("install")
    assert result["added"] == [INSTALL_TEST_PACKAGE], run
    (checked,) = result["checked"]
    assert checked["name"] == INSTALL_TEST_PACKAGE, checked
    assert checked["verdict"] == "PACKAGE_VERDICT_AVAILABLE", checked
    assert checked["version"], checked
    assert checked["derivation"].endswith(".drv"), checked
    (output,) = checked["outputs"]
    assert output["output"] == "out", output
    assert output["storePath"].startswith("/nix/store/"), output
    assert not container.path_exists(output["storePath"]), output
    assert output["substituter"].startswith(mock_nix_server["url"]), output
    assert int(output["narSize"]) > 0, output
    plan = result["plan"]
    assert plan["complete"], plan
    assert plan["paths"] >= 1, plan
    assert int(plan["unpackedBytes"]) >= int(output["narSize"]), plan
