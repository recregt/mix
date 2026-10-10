import json
from dataclasses import dataclass, field


def document_of(stdout: str) -> dict | None:
    """The result document `mix --json` printed, or None when it printed none."""
    text = stdout.strip()
    if not text:
        return None
    return json.loads(text)


@dataclass
class Run:
    returncode: int
    stdout: str
    stderr: str
    document: dict = field(default_factory=dict)

    @property
    def status(self) -> str:
        return self.document.get("status", "STATUS_UNSPECIFIED")

    @property
    def exit_code(self) -> int:
        return int(self.document.get("exit", 0))

    @property
    def request(self) -> str:
        return self.document["request"]

    @property
    def code(self) -> str | None:
        problems = self.document.get("problems", [])
        return problems[0]["code"] if problems else None

    @property
    def cancellation(self) -> str | None:
        found = self.document.get("cancellation", "CANCELLATION_UNSPECIFIED")
        return None if found == "CANCELLATION_UNSPECIFIED" else found

    @property
    def warnings(self) -> list[str]:
        return [warning["code"] for warning in self.document.get("warnings", [])]

    @property
    def waits(self) -> list[dict]:
        return self.document.get("waits", [])

    @property
    def changes(self) -> list[dict]:
        return self.document.get("changes", [])

    def result(self, command: str) -> dict:
        return self.document.get(command, {})

    def steps(self) -> list[str]:
        return [step["key"] for step in self.document.get("steps", [])]

    def rolled_back(self) -> list[str]:
        return [step["key"] for step in self.document.get("steps", []) if step["undone"]]

    def succeeded(self) -> bool:
        return self.status == "STATUS_SUCCEEDED" and self.returncode == 0

    def __repr__(self) -> str:
        if not self.document:
            return f"Run(exited {self.returncode}, no document; stderr:\n{self.stderr})"
        return f"Run(exited {self.returncode}, {self.status}, {self.code}; stderr:\n{self.stderr})"
