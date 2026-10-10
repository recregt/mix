import re
from dataclasses import dataclass

SPAWNS = ("clone", "clone3", "fork", "vfork")
CREDENTIALS = ("setuid", "setreuid", "setresuid", "setfsuid")
EXECS = ("execve", "execveat")
WRITE_FLAGS = ("O_WRONLY", "O_RDWR", "O_CREAT", "O_TRUNC", "O_APPEND")
SYSCALLS = ",".join(
    SPAWNS
    + CREDENTIALS
    + EXECS
    + (
        "open",
        "openat",
        "openat2",
        "creat",
        "truncate",
        "mkdir",
        "mkdirat",
        "rmdir",
        "unlink",
        "unlinkat",
        "rename",
        "renameat",
        "renameat2",
        "link",
        "linkat",
        "symlink",
        "symlinkat",
        "chmod",
        "fchmod",
        "fchmodat",
        "fchmodat2",
        "chown",
        "fchown",
        "lchown",
        "fchownat",
    )
)

LINE = re.compile(r"^(?P<pid>\d+) +(?P<call>\w+)\((?P<args>.*)\) += (?P<ret>-?\d+|\?)")
UNFINISHED = re.compile(r"^(?P<pid>\d+) +(?P<head>.*) <unfinished \.\.\.>$")
RESUMED = re.compile(r"^(?P<pid>\d+) +<\.\.\. \w+ resumed>(?P<tail>.*)$")
STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')
DESCRIPTOR = re.compile(r"^[\w-]*<(?P<path>[^>]*)>")


@dataclass(frozen=True)
class Write:
    pid: int
    uid: int
    call: str
    path: str


@dataclass(frozen=True)
class Call:
    pid: int
    name: str
    args: list[str]
    ret: str


def _split(args: str) -> list[str]:
    parts, current, depth, quoted, escaped = [], "", 0, False, False
    for char in args:
        current += char
        if quoted:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                quoted = False
        elif char == '"':
            quoted = True
        elif char in "{[(":
            depth += 1
        elif char in "}])":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(current[:-1].strip())
            current = ""
    if current.strip():
        parts.append(current.strip())
    return parts


def _calls(trace: str) -> list[Call]:
    pending: dict[str, str] = {}
    calls = []
    for raw in trace.splitlines():
        line = raw
        if match := UNFINISHED.match(line):
            pending[match["pid"]] = match["head"]
            continue
        if match := RESUMED.match(line):
            head = pending.pop(match["pid"], None)
            if head is None:
                continue
            line = f"{match['pid']} {head}{match['tail']}"
        if match := LINE.match(line):
            calls.append(
                Call(
                    int(match["pid"]),
                    match["call"],
                    _split(match["args"]),
                    match["ret"],
                )
            )
    return calls


def _string(arg: str) -> str | None:
    match = STRING.search(arg)
    return match[1] if match else None


def _descriptor(arg: str) -> str | None:
    match = DESCRIPTOR.match(arg)
    return match["path"] if match else None


def _at(directory: str, name: str) -> str | None:
    path = _string(name)
    base = _descriptor(directory)
    if path is None or path == "":
        return base
    if path.startswith("/") or base is None:
        return path
    return f"{base.rstrip('/')}/{path}"


def _writes_flags(flags: str) -> bool:
    return any(flag in flags for flag in WRITE_FLAGS)


def _targets(call: Call) -> list[str | None]:
    name, args = call.name, call.args
    if name == "open":
        return [_string(args[0])] if _writes_flags(args[1]) else []
    if name in ("openat", "openat2"):
        return [_at(args[0], args[1])] if _writes_flags(args[2]) else []
    if name in (
        "creat",
        "truncate",
        "mkdir",
        "rmdir",
        "unlink",
        "chmod",
        "chown",
        "lchown",
    ):
        return [_string(args[0])]
    if name in ("mkdirat", "unlinkat", "fchmodat", "fchmodat2", "fchownat"):
        return [_at(args[0], args[1])]
    if name in ("fchmod", "fchown"):
        return [_descriptor(args[0])]
    if name == "rename":
        return [_string(args[0]), _string(args[1])]
    if name in ("link", "symlink"):
        return [_string(args[1])]
    if name in ("renameat", "renameat2"):
        return [_at(args[0], args[1]), _at(args[2], args[3])]
    if name == "linkat":
        return [_at(args[2], args[3])]
    if name == "symlinkat":
        return [_at(args[1], args[2])]
    return []


def _effective(call: Call, current: int) -> int:
    args = call.args
    if call.name == "setfsuid":
        return int(args[0])
    if call.ret != "0":
        return current
    if call.name == "setuid":
        return int(args[0])
    if call.name in ("setreuid", "setresuid") and args[1] != "-1":
        return int(args[1])
    return current


def writes(trace: str, first_uid: int, setuid_root: set[str]) -> list[Write]:
    calls = _calls(trace)
    parent = {
        int(call.ret): call.pid
        for call in calls
        if call.name in SPAWNS and call.ret.isdigit() and call.ret != "0"
    }
    uid: dict[int, int] = {}

    def inherited(pid: int) -> int:
        if pid not in uid:
            uid[pid] = inherited(parent[pid]) if pid in parent else first_uid
        return uid[pid]

    found = []
    for call in calls:
        current = inherited(call.pid)
        if call.name in CREDENTIALS:
            uid[call.pid] = _effective(call, current)
        elif call.name in EXECS:
            program = _string(call.args[1] if call.name == "execveat" else call.args[0])
            if call.ret == "0" and program in setuid_root:
                uid[call.pid] = 0
        elif call.ret != "?" and not call.ret.startswith("-"):
            found += [
                Write(call.pid, current, call.name, path)
                for path in _targets(call)
                if path is not None
            ]
    return found
