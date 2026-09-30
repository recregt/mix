import os
import re

import pytest

from support.mirror import INSTALL_TEST_PACKAGE, MIRROR_TEST_USERS, bootstrap_root
from support.paths import REPO_ROOT

GOLDEN = REPO_ROOT / "e2e/golden"
UPDATE = os.environ.get("MIX_UPDATE_GOLDEN", "").strip().lower() in ("1", "true", "yes")
USER = MIRROR_TEST_USERS[0]

NORMALIZE = [
    (re.compile(r"/nix/store/[0-9a-z]{32}-"), "/nix/store/<hash>-"),
    (re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b"), "<uuid>"),
    (re.compile(r"host\.containers\.internal:\d+"), "host.containers.internal:<port>"),
]


def _normalized(result) -> str:
    text = f"exit {result.returncode}\n--- stdout\n{result.stdout}--- stderr\n{result.stderr}"
    for pattern, replacement in NORMALIZE:
        text = pattern.sub(replacement, text)
    return text


def _matches_golden(name: str, result) -> None:
    path = GOLDEN / f"{name}.txt"
    observed = _normalized(result)
    if UPDATE:
        path.write_text(observed)
    assert path.exists(), f"{path} is missing; run with MIX_UPDATE_GOLDEN=1"
    assert observed == path.read_text(), f"{name} differs from {path}"


@pytest.mark.verbatim_output
def test_bootstrap_reads_as_recorded(container, mock_nix_server):
    result = bootstrap_root(container, mock_nix_server)

    _matches_golden("bootstrap", result)


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_an_install_reads_as_recorded(container, mock_nix_server, mirror_cache):
    result = container.exec("mix", "--no-progress", "install", INSTALL_TEST_PACKAGE, user=USER)

    _matches_golden("install", result)


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_an_install_of_what_is_there_reads_as_recorded(container, mock_nix_server, mirror_cache):
    result = container.exec("mix", "--no-progress", "install", "git", user=USER)

    _matches_golden("install-already-installed", result)


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_a_refused_install_reads_as_recorded(container, mock_nix_server, mirror_cache):
    result = container.exec("mix", "--no-progress", "install", "not-a-package!", user=USER)

    _matches_golden("install-invalid-name", result)


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_a_remove_reads_as_recorded(container, mock_nix_server, mirror_cache):
    container.exec("mix", "--no-progress", "install", INSTALL_TEST_PACKAGE, user=USER, check=True)

    result = container.exec("mix", "--no-progress", "remove", INSTALL_TEST_PACKAGE, user=USER)

    _matches_golden("remove", result)


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_a_healthy_doctor_reads_as_recorded(container, mock_nix_server, mirror_cache):
    result = container.exec("mix", "--no-progress", "doctor", user=USER)

    _matches_golden("doctor-healthy", result)


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_a_doctor_finding_drift_and_its_repair_read_as_recorded(
    container, mock_nix_server, mirror_cache
):
    container.exec("rm", "/etc/profile.d/mix-nix.sh", check=True)

    doctor = container.exec("mix", "--no-progress", "doctor", user=USER)
    repair = container.exec("mix", "--no-progress", "repair", user=USER)

    _matches_golden("doctor-drift", doctor)
    _matches_golden("repair", repair)


@pytest.mark.bootstrapped
@pytest.mark.verbatim_output
def test_a_verbose_install_reads_as_recorded(container, mock_nix_server, mirror_cache):
    result = container.exec(
        "mix", "--no-progress", "-v", "install", INSTALL_TEST_PACKAGE, user=USER
    )

    _matches_golden("install-verbose", result)
