use mix_core::nix_plan::{
    ALWAYS_LOCAL, BuildPlan, derivation_name, is_always_local, source_builds,
};

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
#[divan::bench(args = [1, 8, 64])]
fn read_the_build_plan(bencher: divan::Bencher, builds: usize) {
    let output = dry_run_output(builds);
    bencher.bench(|| BuildPlan::parse(divan::black_box(&output)));
}

/// A `nix derivation show` document for the planned derivations.
///
/// The attribute set of a real derivation is tens of kilobytes of environment and structured
/// attributes that the answer does not depend on, so the shape matters as much as the count:
/// what is measured here is mostly the cost of skipping.
fn shown_derivations(builds: usize) -> String {
    let filler: Vec<String> = (0..64)
        .map(|i| format!("\"attribute-{i}\":\"/nix/store/{i:032}-input-{i}\""))
        .collect();
    let filler = filler.join(",");

    let entries: Vec<String> = (0..builds)
        .map(|build| {
            let local = build % 2 == 0;
            format!(
                "\"{index:032}-package-{build}.drv\":{{\"name\":\"package-{build}\",\
                 \"env\":{{{filler},\"out\":\"/nix/store/{index:032}-package-{build}\"}},\
                 \"structuredAttrs\":{{{filler},\"preferLocalBuild\":{local},\
                 \"allowSubstitutes\":{substitutes}}}}}",
                index = build,
                substitutes = !local,
            )
        })
        .collect();

    format!(
        "{{\"derivations\":{{{}}},\"version\":4}}",
        entries.join(",")
    )
}

/// Telling home-manager's own generation apart from a package that would be compiled.
#[divan::bench(args = [1, 8, 64])]
fn classify_the_planned_derivations(bencher: divan::Bencher, builds: usize) {
    let planned: Vec<String> = (0..builds)
        .map(|build| store_path(build, &format!("package-{build}"), ".drv"))
        .collect();
    let shown = shown_derivations(builds);

    bencher.bench(|| source_builds(divan::black_box(&planned), divan::black_box(&shown)));
}

/// Dropping the derivations home-manager always renders here, which is what a clean store — or
/// one after a garbage collection — plans on top of the packages being installed.
///
/// This runs before `nix derivation show` is called, so what it saves is not its own cost but
/// the attributes of every entry it removes from that call.
#[divan::bench(args = [1, 8, 64])]
fn drop_the_always_local_derivations(bencher: divan::Bencher, builds: usize) {
    let mut planned: Vec<String> = ALWAYS_LOCAL
        .iter()
        .enumerate()
        .map(|(index, name)| store_path(index, name, ".drv"))
        .collect();
    planned.extend((0..builds).map(|build| {
        store_path(
            build + ALWAYS_LOCAL.len(),
            &format!("package-{build}"),
            ".drv",
        )
    }));

    bencher.bench(|| {
        divan::black_box(&planned)
            .iter()
            .map(String::as_str)
            .filter(|path| !is_always_local(derivation_name(path)))
            .collect::<Vec<&str>>()
    });
}
