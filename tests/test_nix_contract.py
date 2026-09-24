import json
import os

from conftest import NIX_BINARY, bootstrap_root
from support.paths import REPO_ROOT

FIXTURES = REPO_ROOT / "crates/core/fixtures/nix"
PLAN = "/tmp/plan.nix"
ACT_BUILD = 105
UPDATE = os.environ.get("MIX_UPDATE_NIX_FIXTURES", "").strip().lower() in ("1", "true", "yes")


def _with_plan(container, mock_nix_server):
    bootstrap_root(container, mock_nix_server)
    source = (FIXTURES / "plan.nix").read_text()
    container.exec("bash", "-c", f"cat > {PLAN} <<'EOF'\n{source}EOF", check=True)


def _nix(container, *args, stdin=None):
    if stdin is None:
        return container.exec(NIX_BINARY, *args)
    quoted = " ".join(f"'{arg}'" for arg in (NIX_BINARY, *args))
    return container.exec("bash", "-c", f"{quoted} <<'EOF'\n{stdin}\nEOF")


def _fixture(name: str, observed: str) -> str:
    path = FIXTURES / name
    if UPDATE:
        path.write_text(observed)
    return path.read_text()


def _without_warnings(output: str) -> list[str]:
    return [line for line in output.splitlines() if not line.startswith("warning:")]


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


def test_the_dry_run_plan_is_printed_the_way_mix_reads_it(container, mock_nix_server):
    _with_plan(container, mock_nix_server)

    for attribute, fixture in (("generation", "dry-run-many.txt"), ("leaf", "dry-run-one.txt")):
        result = _nix(container, "build", "-f", PLAN, attribute, "--dry-run")

        assert result.returncode == 0, result.stderr
        expected = _fixture(fixture, result.stderr)
        assert _without_warnings(result.stderr) == _without_warnings(expected)


def test_derivations_are_described_the_way_mix_reads_them(container, mock_nix_server):
    _with_plan(container, mock_nix_server)
    dry_run = _nix(container, "build", "-f", PLAN, "generation", "--dry-run")
    assert dry_run.returncode == 0, dry_run.stderr
    planned = [line.strip() for line in dry_run.stderr.splitlines() if line.startswith("  ")]

    result = _nix(container, "derivation", "show", "--stdin", stdin="\n".join(planned))

    assert result.returncode == 0, result.stderr
    shown = json.loads(result.stdout)
    assert shown == json.loads(_fixture("derivation-show.json", result.stdout))
    assert shown["version"] == 4
    for derivation in shown["derivations"].values():
        assert isinstance(derivation["inputs"]["drvs"], dict)
        assert all("path" in output for output in derivation["outputs"].values())

    (profile,) = [
        derivation
        for key, derivation in shown["derivations"].items()
        if key.endswith("-home-manager-path.drv")
    ]
    chosen = profile["structuredAttrs"]["chosenOutputs"]
    assert all(isinstance(group["paths"], list) for group in chosen)


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
