import contextlib
import fcntl
import functools
import json
import math
import os
import select
import statistics
import time
from dataclasses import dataclass
from pathlib import Path

from support.paths import CACHE_DIR, TESTS_ROOT

HISTORY = CACHE_DIR / "resources.jsonl"
BASELINE = TESTS_ROOT / "resources.json"
LEDGER = CACHE_DIR / "admission.json"
LEDGER_LOCK = CACHE_DIR / "admission.lock"
WAKERS = CACHE_DIR / "admission"
CGROUP_ROOT = Path("/sys/fs/cgroup")
PRESSURE_ROOT = Path("/proc/pressure")

KEPT_RUNS = 8
CALIBRATING = os.environ.get("MIX_TEST_CALIBRATE") == "1"


@dataclass(frozen=True)
class Demand:
    peak_bytes: int
    cpu_cores: float
    seconds: float


@functools.cache
def _defined(test: str) -> bool:
    if test.startswith("snapshot::"):
        return True
    path, _, name = test.partition("::")
    try:
        source = (TESTS_ROOT / path).read_text()
    except OSError:
        return False
    return f"def {name}(" in source


def _runs_by_test() -> dict[str, list[dict]]:
    runs: dict[str, list[dict]] = {}
    with contextlib.suppress(FileNotFoundError):
        for line in HISTORY.read_text().splitlines():
            with contextlib.suppress(ValueError, KeyError):
                run = json.loads(line)
                runs.setdefault(run["test"], []).append(run)
    return {test: kept[-KEPT_RUNS:] for test, kept in runs.items() if _defined(test)}


def _baseline() -> dict[str, dict]:
    with contextlib.suppress(FileNotFoundError):
        return json.loads(BASELINE.read_text())
    return {}


def estimates() -> dict[str, Demand]:
    measured = _runs_by_test()
    known: dict[str, Demand] = {}
    for test, entry in _baseline().items():
        if not _defined(test):
            continue
        known[test] = Demand(entry["peak_bytes"], entry["cpu_cores"], entry["seconds"])
    for test, history in measured.items():
        latest = history[-1].get("variant", "fresh")
        runs = [run for run in history if run.get("variant", "fresh") == latest]
        known[test] = Demand(
            peak_bytes=max(run["peak_bytes"] for run in runs),
            cpu_cores=max(run["cpu_seconds"] / run["seconds"] for run in runs),
            seconds=statistics.median(run["seconds"] for run in runs),
        )
    return known


def _snapshot_path(session: str) -> Path:
    return CACHE_DIR / f"estimates-{session}.json"


def snapshot(session: str) -> None:
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    for stale in CACHE_DIR.glob("estimates-*.json"):
        stale.unlink(missing_ok=True)
    _snapshot_path(session).write_text(
        json.dumps({test: vars(demand) for test, demand in estimates().items()})
    )


def snapshotted(session: str) -> dict[str, Demand]:
    with contextlib.suppress(FileNotFoundError, ValueError):
        return {
            test: Demand(**demand)
            for test, demand in json.loads(_snapshot_path(session).read_text()).items()
        }
    return estimates()


def demand_for(test: str, known: dict[str, Demand]) -> Demand | None:
    if test in known:
        return known[test]
    if not known:
        return None
    return Demand(
        peak_bytes=max(demand.peak_bytes for demand in known.values()),
        cpu_cores=max(demand.cpu_cores for demand in known.values()),
        seconds=max(demand.seconds for demand in known.values()),
    )


def _own_cgroup() -> Path:
    for line in Path("/proc/self/cgroup").read_text().splitlines():
        if line.startswith("0::"):
            return CGROUP_ROOT / line[3:].lstrip("/")
    return CGROUP_ROOT


def _ancestors(cgroup: Path):
    while cgroup != CGROUP_ROOT and CGROUP_ROOT in cgroup.parents:
        yield cgroup
        cgroup = cgroup.parent


def _read(path: Path) -> str | None:
    with contextlib.suppress(OSError):
        return path.read_text().strip()
    return None


def cpu_capacity() -> float:
    capacity = float(len(os.sched_getaffinity(0)))
    for cgroup in _ancestors(_own_cgroup()):
        limit = _read(cgroup / "cpu.max")
        if limit and not limit.startswith("max"):
            quota, period = limit.split()
            capacity = min(capacity, int(quota) / int(period))
    return capacity


def memory_available() -> int:
    available = 0
    for line in Path("/proc/meminfo").read_text().splitlines():
        if line.startswith("MemAvailable:"):
            available = int(line.split()[1]) * 1024
    for cgroup in _ancestors(_own_cgroup()):
        limit = _read(cgroup / "memory.max")
        current = _read(cgroup / "memory.current")
        if limit and limit != "max" and current:
            available = min(available, int(limit) - int(current))
    return available


def worker_cap() -> int:
    known = estimates()
    if not known:
        return 1
    demands = known.values()
    seconds = sum(demand.seconds for demand in demands)
    longest = max(demand.seconds for demand in demands)
    cores = sum(demand.cpu_cores * demand.seconds for demand in demands) / seconds
    memory = sum(demand.peak_bytes * demand.seconds for demand in demands) / seconds
    return max(
        1,
        min(
            len(known),
            math.ceil(seconds / longest),
            math.floor(cpu_capacity() / cores),
            math.floor(memory_available() / memory),
        ),
    )


def _load_ledger() -> dict:
    with contextlib.suppress(FileNotFoundError, ValueError):
        return json.loads(LEDGER.read_text())
    return {"running": {}, "waiting": [], "memory": 0}


@contextlib.contextmanager
def _locked():
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(LEDGER_LOCK, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        ledger = _load_ledger()
        ledger.setdefault("memory", 0)
        ledger["running"] = {
            name: entry for name, entry in ledger["running"].items() if alive(entry["pid"])
        }
        ledger["waiting"] = [entry for entry in ledger["waiting"] if alive(entry["pid"])]
        yield ledger
        LEDGER.write_text(json.dumps(ledger))


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _fits(ledger: dict, demand: Demand | None) -> bool:
    running = ledger["running"].values()
    if not running:
        return True
    if demand is None:
        return CALIBRATING
    reserved = sum(entry["peak_bytes"] for entry in running)
    cores = sum(entry["cpu_cores"] for entry in running)
    return (
        reserved + demand.peak_bytes <= ledger["memory"]
        and cores + demand.cpu_cores <= cpu_capacity()
    )


def _waker(ticket: str) -> Path:
    return WAKERS / ticket


def _wake_waiters(ledger: dict) -> None:
    for entry in ledger["waiting"]:
        with contextlib.suppress(OSError):
            waker = os.open(_waker(entry["ticket"]), os.O_WRONLY | os.O_NONBLOCK)
            with contextlib.suppress(BlockingIOError):
                os.write(waker, b"\n")
            os.close(waker)


def _watch(ledger: dict) -> list[int]:
    """Pidfds of every other process in the ledger, readable once that process ends."""
    watched = []
    pids = {entry["pid"] for entry in [*ledger["running"].values(), *ledger["waiting"]]}
    for pid in pids - {os.getpid()}:
        with contextlib.suppress(ProcessLookupError):
            watched.append(os.pidfd_open(pid))
    return watched


def admit(ticket: str, test: str, demand: Demand | None) -> float:
    """Returns once `test` may start, in arrival order, with the seconds it waited.

    A test is admitted when the peaks of everything running plus its own fit the memory that was
    available when nothing of ours ran, and their cores fit the CPUs. Whether it fits changes
    only when a test starts, ends or dies, so a waiter sleeps until one of those happens: a
    release writes to every waiter's FIFO, and a death closes the pidfd it holds.
    """
    started = time.monotonic()
    WAKERS.mkdir(parents=True, exist_ok=True)
    waker = _waker(ticket)
    os.mkfifo(waker)
    wakes = os.open(waker, os.O_RDWR | os.O_NONBLOCK)
    try:
        with _locked() as ledger:
            ledger["waiting"].append({"ticket": ticket, "pid": os.getpid()})
        while True:
            with _locked() as ledger:
                if not ledger["running"]:
                    ledger["memory"] = memory_available()
                first = ledger["waiting"][0]["ticket"] == ticket
                if first and _fits(ledger, demand):
                    ledger["waiting"].pop(0)
                    ledger["running"][ticket] = {
                        "test": test,
                        "pid": os.getpid(),
                        "peak_bytes": demand.peak_bytes if demand else 0,
                        "cpu_cores": demand.cpu_cores if demand else 0.0,
                    }
                    _wake_waiters(ledger)
                    return time.monotonic() - started
                watched = _watch(ledger)
            try:
                select.select([wakes, *watched], [], [])
                with contextlib.suppress(BlockingIOError):
                    os.read(wakes, 4096)
            finally:
                for pidfd in watched:
                    os.close(pidfd)
    except BaseException:
        release(ticket)
        raise
    finally:
        os.close(wakes)
        waker.unlink(missing_ok=True)


def release(ticket: str) -> None:
    with _locked() as ledger:
        ledger["running"].pop(ticket, None)
        ledger["waiting"] = [entry for entry in ledger["waiting"] if entry["ticket"] != ticket]
        _wake_waiters(ledger)


def _refused_forks(cgroup: Path) -> int:
    refused = 0
    for directory, _, files in os.walk(cgroup):
        if "pids.events" in files:
            for line in (_read(Path(directory) / "pids.events") or "").splitlines():
                if line.startswith("max "):
                    refused += int(line.split()[1])
    return refused


def record(test: str, cgroup: Path, seconds: float, waited: float, variant: str) -> None:
    peak = _read(cgroup / "memory.peak")
    tasks = _read(cgroup / "pids.peak")
    refused = _refused_forks(cgroup)
    stat = _read(cgroup / "cpu.stat") or ""
    usage = next(
        (int(line.split()[1]) for line in stat.splitlines() if line.startswith("usage_usec")),
        None,
    )
    if peak is None or usage is None or seconds <= 0:
        return
    run = {
        "test": test,
        "session": os.environ.get("MIX_TEST_SESSION", ""),
        "peak_bytes": int(peak),
        "peak_tasks": int(tasks) if tasks else None,
        "refused_forks": refused,
        "cpu_seconds": usage / 1_000_000,
        "seconds": seconds,
        "waited_seconds": waited,
        "variant": variant,
    }
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(HISTORY, "a") as history:
        fcntl.flock(history, fcntl.LOCK_EX)
        history.write(json.dumps(run) + "\n")


def trim_history() -> None:
    runs = _runs_by_test()
    lines = [json.dumps(run) for kept in runs.values() for run in kept]
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    with open(HISTORY, "a+") as history:
        fcntl.flock(history, fcntl.LOCK_EX)
        history.seek(0)
        history.truncate()
        history.write("".join(line + "\n" for line in lines))


def session_runs(session: str) -> list[dict]:
    return [
        run for kept in _runs_by_test().values() for run in kept if run.get("session") == session
    ]


def pressure() -> dict[str, int]:
    totals = {}
    for resource in ("cpu", "memory", "io"):
        text = _read(PRESSURE_ROOT / resource) or ""
        for line in text.splitlines():
            kind, *fields = line.split()
            for field in fields:
                if field.startswith("total="):
                    totals[f"{resource} {kind}"] = int(field[len("total=") :])
    return totals


def write_baseline() -> None:
    known = {test: demand for test, demand in estimates().items() if test in _runs_by_test()}
    BASELINE.write_text(
        json.dumps(
            {
                test: {
                    "peak_bytes": demand.peak_bytes,
                    "cpu_cores": round(demand.cpu_cores, 3),
                    "seconds": round(demand.seconds, 1),
                }
                for test, demand in sorted(known.items())
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    write_baseline()
