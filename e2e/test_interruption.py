from support.mirror import silent_mirror

PROVISIONING_MANIFEST = "/nix/.mix-provisioning-manifest"
DEFAULT_PROFILE_NIX_ENV = "/nix/var/nix/profiles/default/bin/nix-env"


def test_hard_kill_during_fetch_and_unpack_then_bootstrap_converges(
    container, mock_nix_server
):
    with silent_mirror() as silent:
        bootstrap = container.mix_background(
            "bootstrap", env={"MIX_NIX_MIRROR": silent}
        )
        bootstrap.wait_for_step("fetch-runtime")
        worker = container.exec(
            "pgrep", "-f", "^/usr/local/bin/mix-daemon serve-stdin", check=True
        ).stdout.split()[0]
        container.exec("kill", "-KILL", worker, check=True)

        orphaned = bootstrap.wait(timeout=15.0, complete=False)
    assert orphaned.returncode == 1, orphaned

    fix = container.mix("bootstrap", env={"MIX_NIX_MIRROR": mock_nix_server["url"]})
    assert fix.succeeded(), fix

    assert container.path_exists(DEFAULT_PROFILE_NIX_ENV)
    assert not container.path_exists(PROVISIONING_MANIFEST)
    assert container.mix("doctor").succeeded()
