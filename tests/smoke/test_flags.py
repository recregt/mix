import pytest


@pytest.mark.parametrize("verbose_flags", [[], ["-v"], ["-vv"], ["-vvv"]])
def test_bootstrap_is_idempotent_at_every_verbosity_level(bootstrapped_container, verbose_flags):
    result = bootstrapped_container.exec("mix", *verbose_flags, "bootstrap")
    assert result.returncode == 0, result.stderr


def test_repair_is_a_clean_no_op_on_a_healthy_system(bootstrapped_container):
    result = bootstrapped_container.exec("mix", "repair")
    assert result.returncode == 0, result.stderr


def test_bootstrap_accepts_a_real_mirror_url(fresh_container, nix_mirror_base):
    result = fresh_container.exec("mix", "bootstrap", "--mirror", nix_mirror_base)
    assert result.returncode == 0, result.stderr
