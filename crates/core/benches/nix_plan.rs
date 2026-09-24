use mix_core::nix_plan::{BuildPlan, classify};

fn main() {
    divan::main();
}

fn store_path(index: usize, name: &str, suffix: &str) -> String {
    format!("/nix/store/{index:032}-{name}{suffix}")
}

/// Output shaped like the one `nix build --dry-run` writes before an install: a short list of
/// derivations to build and a long list of paths to fetch, which the parser has to walk past.
fn dry_run_output(builds: usize) -> String {
    let mut output = String::from("warning: Git tree '/home/mix/.local/state/mix' is dirty\n");
    output.push_str(&format!("these {builds} derivations will be built:\n"));
    for build in 0..builds {
        output.push_str(&format!("  {}\n", store_path(build, "package", ".drv")));
    }
    output.push_str("these 169 paths will be fetched (241.8 MiB download, 799.2 MiB unpacked):\n");
    for fetched in 0..169 {
        output.push_str(&format!("  {}\n", store_path(fetched, "fetched", "")));
    }
    output
}

/// Reading the plan out of a dry run: one pass over the output, copying only the paths.
#[divan::bench(args = [1, 64, 609])]
fn read_the_build_plan(bencher: divan::Bencher, builds: usize) {
    let output = dry_run_output(builds);
    bencher.bench(|| BuildPlan::parse(divan::black_box(&output)));
}

fn shown_derivations(builds: usize) -> String {
    let filler: Vec<String> = (0..64)
        .map(|i| format!("\"attribute-{i}\":\"/nix/store/{i:032}-input-{i}\""))
        .collect();
    let filler = filler.join(",");

    let entries: Vec<String> = (0..builds)
        .map(|build| {
            let local = build % 2 == 0;
            let inputs: Vec<String> = [build + 1, build * 2 + 1, build * 3 + 1]
                .into_iter()
                .filter(|&input| input < builds)
                .map(|input| {
                    format!(
                        "\"{input:032}-package-{input}.drv\":{{\"dynamicOutputs\":{{}},\"outputs\":[\"out\"]}}"
                    )
                })
                .collect();
            format!(
                "\"{index:032}-package-{build}.drv\":{{\"name\":\"package-{build}\",\
                 \"env\":{{{filler},\"out\":\"/nix/store/{index:032}-package-{build}\"}},\
                 \"inputs\":{{\"drvs\":{{{inputs}}},\"srcs\":[]}},\
                 \"structuredAttrs\":{{{filler},\"preferLocalBuild\":{local},\
                 \"allowSubstitutes\":{substitutes}}}}}",
                index = build,
                inputs = inputs.join(","),
                substitutes = !local,
            )
        })
        .collect();

    format!(
        "{{\"derivations\":{{{}}},\"version\":4}}",
        entries.join(",")
    )
}

#[divan::bench(args = [1, 64, 609])]
fn classify_the_planned_derivations(bencher: divan::Bencher, builds: usize) {
    let planned: Vec<String> = (0..builds)
        .map(|build| store_path(build, &format!("package-{build}"), ".drv"))
        .collect();
    let shown = shown_derivations(builds);

    bencher.bench(|| classify(divan::black_box(&planned), divan::black_box(&shown)));
}
