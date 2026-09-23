import re

import pytest

from conftest import bootstrap_root


def _discover_subcommands(container):
    result = container.exec("mix", "--help")
    assert result.returncode == 0, result.stderr

    match = re.search(r"Commands:\n(.*?)\n\n", result.stdout, re.S)
    assert match, f"no Commands section found in --help output:\n{result.stdout}"

    names = [line.split()[0] for line in match.group(1).splitlines() if line.strip()]
    return [name for name in names if name != "help"]


def test_top_level_help_lists_at_least_one_subcommand(container):
    assert _discover_subcommands(container)


def test_every_discovered_subcommand_has_working_help(container):
    for name in _discover_subcommands(container):
        result = container.exec("mix", name, "--help")
        assert result.returncode == 0, f"`mix {name} --help` failed: {result.stderr}"
        assert result.stdout.strip(), f"`mix {name} --help` produced no output"


@pytest.mark.parametrize("verbose_flags", [[], ["-v"], ["-vv"], ["-vvv"]])
def test_bootstrap_is_idempotent_at_every_verbosity_level(container, mock_nix_server, verbose_flags):
    bootstrap_root(container, mock_nix_server)

    result = container.exec("mix", *verbose_flags, "bootstrap", "--mirror", mock_nix_server["url"])

    assert result.returncode == 0, result.stderr
