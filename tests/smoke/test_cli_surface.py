import re


def _discover_subcommands(container):
    result = container.exec("mix", "--help")
    assert result.returncode == 0, result.stderr

    match = re.search(r"Commands:\n(.*?)\n\n", result.stdout, re.S)
    assert match, f"no Commands section found in --help output:\n{result.stdout}"

    names = [line.split()[0] for line in match.group(1).splitlines() if line.strip()]
    return [name for name in names if name != "help"]


def test_top_level_help_lists_at_least_one_subcommand(fresh_container):
    assert _discover_subcommands(fresh_container)


def test_every_discovered_subcommand_has_working_help(fresh_container):
    for name in _discover_subcommands(fresh_container):
        result = fresh_container.exec("mix", name, "--help")
        assert result.returncode == 0, f"`mix {name} --help` failed: {result.stderr}"
        assert result.stdout.strip(), f"`mix {name} --help` produced no output"
