use std::path::Path;

use mix_nixgen::{FlakeConfig, HomeModule, Rev, StateVersion, System};
use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

const NIXPKGS: Rev = Rev::new_static(NIXPKGS_REV);
const HOME_MANAGER: Rev = Rev::new_static(HOME_MANAGER_REV);
const STATE_VERSION: StateVersion = StateVersion::new_static("24.05");

fn main() {
    divan::main();
}

fn package_names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("package-{i}")).collect()
}

#[divan::bench(args = [1, 8, 64])]
fn render_a_package_list(bencher: divan::Bencher, n: usize) {
    bencher
        .with_inputs(|| {
            HomeModule::new("mix", Path::new("/home/mix"), STATE_VERSION)
                .unwrap()
                .packages(package_names(n))
                .unwrap()
        })
        .bench_values(|cfg| cfg.render());
}

#[divan::bench]
fn render_a_flake(bencher: divan::Bencher) {
    bencher
        .with_inputs(|| {
            FlakeConfig::new(System::X86_64Linux, "mix", NIXPKGS, HOME_MANAGER).unwrap()
        })
        .bench_values(|cfg| cfg.render());
}

#[divan::bench]
fn build_a_flake_with_an_escape_heavy_username(bencher: divan::Bencher) {
    bencher.bench(|| {
        FlakeConfig::new(
            divan::black_box(System::X86_64Linux),
            divan::black_box("quote \" and interpolation ${x} and \n a newline"),
            NIXPKGS,
            HOME_MANAGER,
        )
        .unwrap()
    });
}
