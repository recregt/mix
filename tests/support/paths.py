import os
import pathlib

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
TESTS_ROOT = pathlib.Path(__file__).resolve().parents[1]
CACHE_DIR = pathlib.Path(os.environ.get("XDG_CACHE_HOME", pathlib.Path.home() / ".cache")) / "mix-bootstrap-tests"
