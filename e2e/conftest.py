import pytest

pytest.register_assert_rewrite("support")

from support.mirror import start_mirror_server

pytest_plugins = ["support.container", "support.mirror"]

MIRROR_SERVER = pytest.StashKey[object]()


def _is_controller(config) -> bool:
    return not hasattr(config, "workerinput")


def pytest_configure(config):
    if _is_controller(config):
        config.stash[MIRROR_SERVER] = start_mirror_server()


def pytest_unconfigure(config):
    if _is_controller(config) and MIRROR_SERVER in config.stash:
        config.stash[MIRROR_SERVER].shutdown()
