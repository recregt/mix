import pytest
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]
REPOSITORY = f"/home/{USER}/.local/state/mix/.git"


def _findings(run) -> dict[str, dict]:
    return {
        report["target"]: report["finding"]
        for report in run.result("doctor").get("reports", [])
        if "finding" in report
    }


@pytest.mark.bootstrapped
def test_a_damaged_repository_is_found_and_replaced_so_installs_work_again(
    container, mock_nix_server, mirror_cache
):
    container.exec("bash", "-c", f"echo garbage > {REPOSITORY}/HEAD", user=USER, check=True)

    found = container.mix("doctor", user=USER)
    assert found.exit_code == 3, found
    assert _findings(found) == {REPOSITORY: {"repositoryBroken": {}}}

    repaired = container.mix("repair", user=USER)
    assert repaired.succeeded(), repaired

    assert container.mix("doctor", user=USER).exit_code == 0
    installed = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert installed.succeeded(), installed
    leftovers = container.exec("ls", "-A", f"/home/{USER}/.local/state/mix", check=True).stdout
    assert "mix-aside" not in leftovers


@pytest.mark.bootstrapped
def test_a_repository_root_wrote_into_is_given_back_with_its_history(
    container, mock_nix_server, mirror_cache
):
    def commits() -> str:
        return container.exec(
            f"/home/{USER}/.nix-profile/bin/git", "--git-dir", REPOSITORY, "rev-list", "--count", "HEAD", user=USER, check=True
        ).stdout

    before = commits()
    container.exec(
        "bash", "-c",
        f"mkdir -p {REPOSITORY}/objects/zz && touch {REPOSITORY}/objects/zz/root",
        check=True,
    )

    found = container.mix("doctor", user=USER)
    assert found.exit_code == 3, found
    (finding,) = _findings(found).values()
    assert "owner" in finding, finding

    assert container.mix("repair", user=USER).succeeded()
    strangers = container.exec(
        "find", REPOSITORY, "!", "-user", USER, check=True
    ).stdout
    assert strangers == ""
    assert commits() == before
    assert container.mix("doctor", user=USER).exit_code == 0
    installed = container.mix("install", INSTALL_TEST_PACKAGE, user=USER)
    assert installed.succeeded(), installed
