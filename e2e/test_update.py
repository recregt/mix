import pytest

from support.mirror import MIRROR_TEST_USERS

USER = MIRROR_TEST_USERS[0]
STATE_DIR = f"/home/{USER}/.local/state/mix"


def _managed_files(container):
    return {
        name: container.exec("cat", f"{STATE_DIR}/{name}", check=True).stdout
        for name in ("flake.nix", "flake.lock", "home.nix", "state")
    }


@pytest.mark.bootstrapped
def test_a_machine_that_fetches_from_a_mirror_cannot_update_and_nothing_changes(
    container, mock_nix_server, mirror_cache
):
    before = _managed_files(container)

    run = container.mix("update", user=USER)

    assert run.exit_code == 1, run
    assert run.code == "CODE_UPDATE_UNAVAILABLE", run
    assert _managed_files(container) == before


@pytest.mark.bootstrapped
def test_a_dry_run_update_on_a_mirrored_machine_is_refused_the_same_way(
    container, mock_nix_server, mirror_cache
):
    before = _managed_files(container)

    run = container.mix("update", "--dry-run", user=USER)

    assert run.exit_code == 1, run
    assert run.code == "CODE_UPDATE_UNAVAILABLE", run
    assert _managed_files(container) == before
