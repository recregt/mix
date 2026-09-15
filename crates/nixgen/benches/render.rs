use mix_nixgen::HomeManagerConfig;

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
