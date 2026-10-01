from support.container import create_user
from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, mirror_args

USER = MIRROR_TEST_USERS[0]
HOME = f"/home/{USER}"
HOME_NIX = f"{HOME}/.local/state/mix/home.nix"


def _traced(container, *args):
    run, writes = container.traced_mix(*args, user=USER)
    assert run.succeeded(), run
    under_home = [write for write in writes if write.path.startswith(f"{HOME}/")]
    assert under_home, (
        f"`mix {args[0]}` wrote nothing under {HOME}, so the trace proves nothing"
    )
    as_root = [write for write in under_home if write.uid == 0]
    assert not as_root, "\n".join(f"{write.call} {write.path}" for write in as_root)
    return run, writes


def test_no_command_writes_into_a_home_directory_as_root(
    container, mock_nix_server, mirror_cache
):
    create_user(container, USER, sudo=True)

    _, writes = _traced(
        container, "bootstrap", *mirror_args(mock_nix_server, mirror_cache)
    )
    assert any(write.uid == 0 and write.path.startswith("/nix/") for write in writes), (
        "the trace saw no root writes, so it cannot tell root from the user"
    )
    _traced(container, "install", INSTALL_TEST_PACKAGE)
    _traced(container, "remove", INSTALL_TEST_PACKAGE)

    container.exec("sh", "-c", f"echo '{{ }}' > {HOME_NIX}", user=USER, check=True)
    repair, _ = _traced(container, "repair")
    fixed = [
        report["target"]
        for report in repair.result("repair")["reports"]
        if report.get("fixed")
    ]
    assert HOME_NIX in fixed, repair
