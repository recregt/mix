use mix_nixgen::{FlakeConfig, HomeManagerConfig};

const NIXPKGS_REV: &str = "efe6f071ede9d21c37462d2d6682d5e670099684";
const HOME_MANAGER_REV: &str = "efa3ccb4c3cc90d832eab232976379058fa75aa3";

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
            FlakeConfig::new("x86_64-linux", "mix", NIXPKGS_REV, HOME_MANAGER_REV).unwrap()
        })
        .bench_values(|cfg| cfg.render());
}

#[divan::bench]
fn build_a_flake_with_an_escape_heavy_username(bencher: divan::Bencher) {
    bencher.bench(|| {
        FlakeConfig::new(
            divan::black_box("x86_64-linux"),
            divan::black_box("quote \" and interpolation ${x} and \n a newline"),
            NIXPKGS_REV,
            HOME_MANAGER_REV,
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
