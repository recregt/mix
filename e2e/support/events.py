import json
from dataclasses import dataclass, field

ROOT = "1"


def envelopes_of(capture: str) -> list[dict]:
    envelopes = []
    for line in capture.splitlines():
        if not line.strip():
            continue
        entry = json.loads(line)
        if "record" in entry:
            envelopes.append(entry["record"]["envelope"])
    return envelopes


def started_step(envelope: dict, key: str) -> bool:
    started = envelope.get("nodeStarted", {})
    return "step" in started and started.get("key") == key


def progress_of(envelope: dict, kind: str) -> dict | None:
    return envelope.get("nodeProgress", {}).get(kind)


@dataclass
class Run:
    returncode: int
    stdout: str
    stderr: str
    envelopes: list[dict] = field(default_factory=list)

    @property
    def root(self) -> dict:
        (finished,) = [
            envelope["nodeFinished"]
            for envelope in self.envelopes
            if envelope.get("nodeFinished", {}).get("id") == ROOT
        ]
        return finished

    @property
    def status(self) -> str:
        return self.root.get("status", "STATUS_UNSPECIFIED")

    @property
    def exit_code(self) -> int:
        return int(self.root.get("exitCode", 0))

    def result(self, command: str) -> dict:
        return self.root.get(command, {})

    @property
    def code(self) -> str | None:
        return self.root.get("diagnostic", {}).get("code")

    @property
    def cancellation(self) -> str | None:
        return self.root.get("cancellation")

    @property
    def warnings(self) -> list[str]:
        return [
            envelope["diagnostic"].get("code", "CODE_UNSPECIFIED")
            for envelope in self.envelopes
            if "diagnostic" in envelope
        ]

    def progress(self, kind: str) -> list[dict]:
        return [
            found
            for envelope in self.envelopes
            if (found := progress_of(envelope, kind)) is not None
        ]

    def steps(self) -> list[str]:
        return [
            envelope["nodeStarted"]["key"]
            for envelope in self.envelopes
            if "step" in envelope.get("nodeStarted", {})
        ]

    def rolled_back(self) -> list[str]:
        undone = {
            envelope["nodeStarted"]["rollback"].get("undoes")
            for envelope in self.envelopes
            if "rollback" in envelope.get("nodeStarted", {})
        }
        return [
            envelope["nodeStarted"]["key"]
            for envelope in self.envelopes
            if "step" in envelope.get("nodeStarted", {})
            and envelope["nodeStarted"]["id"] in undone
        ]

    def succeeded(self) -> bool:
        return self.status == "STATUS_SUCCEEDED" and self.returncode == 0

    def __repr__(self) -> str:
        finished = [
            e for e in self.envelopes if e.get("nodeFinished", {}).get("id") == ROOT
        ]
        outcome = finished[0] if finished else None
        return (
            f"Run(returncode={self.returncode}, root={outcome}, steps={self.steps()})\n"
            f"--- stderr\n{self.stderr}"
        )
