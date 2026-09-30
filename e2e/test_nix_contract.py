import json
import os

from support.container import NIX_BINARY
from support.mirror import bootstrap_root
from support.paths import REPO_ROOT

FIXTURES = REPO_ROOT / "crates/core/fixtures/nix"
PLAN = "/tmp/plan.nix"
ACT_BUILD = 105
UPDATE = os.environ.get("MIX_UPDATE_NIX_FIXTURES", "").strip().lower() in ("1", "true", "yes")


def _with_plan(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)
    source = (FIXTURES / "plan.nix").read_text()
    container.exec("bash", "-c", f"cat > {PLAN} <<'EOF'\n{source}EOF", check=True)


def _nix(container, *args):
    return container.exec(NIX_BINARY, *args)


def _fixture(name: str, observed: str) -> str:
    path = FIXTURES / name
    if UPDATE:
        path.write_text(observed)
    return path.read_text()


def _build_starts(output: str) -> list[dict]:
    records = [
        json.loads(line.removeprefix("@nix "))
        for line in output.splitlines()
        if line.startswith("@nix ")
    ]
    return [
        record
        for record in records
        if record["action"] == "start" and record["type"] == ACT_BUILD
    ]


def test_a_build_starting_is_logged_the_way_mix_reads_it(container, mock_nix_server):
    _with_plan(container, mock_nix_server)
    drv_path = _nix(container, "eval", "--raw", "-f", PLAN, "failing.drvPath").stdout.strip()

    result = _nix(
        container, "build", "-f", PLAN, "failing", "--no-link", "--log-format", "internal-json"
    )

    starts = _build_starts(result.stderr)
    assert [record["fields"][0] for record in starts] == [drv_path]

    (expected,) = _build_starts(_fixture("build-log.txt", result.stderr))
    assert sorted(starts[0]) == sorted(expected)
    assert [type(field) for field in starts[0]["fields"]] == [
        type(field) for field in expected["fields"]
    ]
