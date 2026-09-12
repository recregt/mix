def test_doctor_reports_the_bootstrapped_environment_as_healthy(bootstrapped_container):
    result = bootstrapped_container.exec("mix", "doctor")
    assert result.returncode == 0, result.stdout


def test_nix_is_installed_and_on_path(bootstrapped_container):
    result = bootstrapped_container.nix_exec("nix", "--version")
    assert result.returncode == 0, result.stderr
    assert "Nix" in result.stdout


def test_profile_install_fetches_and_runs_a_real_package(bootstrapped_container):
    install = bootstrapped_container.nix_exec("nix", "profile", "install", "nixpkgs#hello")
    assert install.returncode == 0, install.stderr

    run = bootstrapped_container.nix_exec("hello")
    assert run.returncode == 0, run.stderr
    assert run.stdout.strip() == "Hello, world!"


def test_home_manager_flake_builds_and_runs(bootstrapped_container):
    result = bootstrapped_container.nix_exec(
        "nix", "run", "home-manager/master", "--", "--version"
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip()
