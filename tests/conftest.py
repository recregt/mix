import math
import os
import uuid

import pytest

pytest.register_assert_rewrite("support")

from support import resources
from support.container import reap_orphans, remove_snapshot
from support.mirror import start_mirror_server

pytest_plugins = ["support.container", "support.mirror"]

PRESSURE_AT_START = pytest.StashKey[dict]()
MIRROR_SERVER = pytest.StashKey[object]()


def _is_controller(config) -> bool:
    return not hasattr(config, "workerinput")


def pytest_configure(config):
    if _is_controller(config):
        reap_orphans()
        os.environ.setdefault("MIX_TEST_CONTROLLER_PID", str(os.getpid()))
        session = os.environ.setdefault("MIX_TEST_SESSION", uuid.uuid4().hex)
        resources.snapshot(session)
        config.stash[MIRROR_SERVER] = start_mirror_server()
        config.stash[PRESSURE_AT_START] = resources.pressure()


def pytest_unconfigure(config):
    if _is_controller(config) and MIRROR_SERVER in config.stash:
        config.stash[MIRROR_SERVER].shutdown()
        remove_snapshot()


@pytest.hookimpl(optionalhook=True)
def pytest_xdist_auto_num_workers(config):
    return resources.worker_cap()


def pytest_collection_modifyitems(items):
    known = resources.snapshotted(os.environ.get("MIX_TEST_SESSION", ""))
    items.sort(key=lambda item: -known[item.nodeid].seconds if item.nodeid in known else -math.inf)


def pytest_terminal_summary(terminalreporter, config):
    if not _is_controller(config):
        return
    runs = resources.session_runs(os.environ.get("MIX_TEST_SESSION", ""))
    resources.trim_history()
    if not runs:
        return
    start = config.stash.get(PRESSURE_AT_START, {})
    stalls = ", ".join(
        f"{name} {(total - start.get(name, 0)) / 1_000_000:.1f}s"
        for name, total in resources.pressure().items()
    )
    gib = 1024**3
    terminalreporter.write_sep("-", "resources")
    terminalreporter.write_line(
        f"{len(runs)} containers measured; peak memory per test up to "
        f"{max(run['peak_bytes'] for run in runs) / gib:.2f} GiB; "
        f"CPU per test up to {max(run['cpu_seconds'] / run['seconds'] for run in runs):.2f} cores; "
        f"admission waits {sum(run['waited_seconds'] for run in runs):.0f}s in total; "
        f"tasks per test up to {max(run.get('peak_tasks') or 0 for run in runs)}; "
        f"forks refused by a task limit {sum(run.get('refused_forks') or 0 for run in runs)}"
    )
    terminalreporter.write_line(
        f"capacity: {resources.cpu_capacity():g} cores, "
        f"{resources.memory_available() / gib:.1f} GiB available now"
    )
    terminalreporter.write_line(f"pressure stalls during the run: {stalls}")
