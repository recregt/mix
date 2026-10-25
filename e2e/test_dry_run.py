import pytest

from support.container import create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, mirror_args

USER = MIRROR_TEST_USERS[0]
DISCARDED = "/dev/null"
NIX_CACHE = f"/home/{USER}/.cache/nix/"


def test_a_dry_run_of_a_fresh_bootstrap_writes_nothing(container, mock_nix_server, mirror_cache):
    create_user(container, USER, sudo=True)

    run, writes = container.traced_mix(
        "bootstrap", "-n", *mirror_args(mock_nix_server, mirror_cache), user=USER
    )

    assert run.succeeded(), run
    assert run.document["dryRun"], run
    assert run.changes, run
    assert writes == [], writes


@pytest.mark.bootstrapped
def test_a_dry_run_of_every_command_writes_nothing_but_nix_s_own_cache(
    container, mock_nix_server, mirror_cache
):
    with container.traced_daemon() as traced:
        runs = [
            container.mix("install", "-n", INSTALL_TEST_PACKAGE, user=USER),
            container.mix("remove", "-n", INSTALL_TEST_PACKAGE, user=USER),
            container.mix("clean", "-n", user=USER),
            container.mix("repair", "-n", user=USER),
        ]

    for run in runs:
        assert run.succeeded(), run
        assert run.document["dryRun"], run
    assert runs[0].changes, runs[0]
    changed = [
        write
        for write in traced.writes
        if write.path != DISCARDED and not write.path.startswith(NIX_CACHE)
    ]
    assert changed == [], changed
