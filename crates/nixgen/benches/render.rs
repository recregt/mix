use mix_nixgen::{FlakeConfig, HomeManagerConfig, Rev, System};
use mix_pins::{HOME_MANAGER_REV, NIXPKGS_REV};

const NIXPKGS: Rev = Rev::new_static(NIXPKGS_REV);
const HOME_MANAGER: Rev = Rev::new_static(HOME_MANAGER_REV);

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
            let mut cfg = HomeManagerConfig::new();
            cfg.packages(package_names(n)).unwrap();
            cfg
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

#[divan::bench(args = [1, 8, 64])]
fn render_many_nested_program_options(bencher: divan::Bencher, n: usize) {
    bencher
        .with_inputs(|| {
            let mut cfg = HomeManagerConfig::new();
            for i in 0..n {
                cfg.set_bool(&format!("programs.app{i}.enable"), true)
                    .unwrap();
                cfg.set_str(
                    &format!("programs.app{i}.note"),
                    "quote \" and interpolation ${x} and \n a newline",
                )
                .unwrap();
            }
            cfg
        })
        .bench_values(|cfg| cfg.render());
}
