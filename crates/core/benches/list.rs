use mix_core::ops::list::{Described, Stored, join};

fn main() {
    divan::main();
}

fn names(count: usize) -> Vec<String> {
    (0..count).map(|n| format!("package-{n:03}")).collect()
}

fn described(names: &[String]) -> Vec<Described> {
    names
        .iter()
        .map(|name| Described {
            name: name.clone(),
            pname: Some(name.clone()),
            version: Some("1.2.3".to_string()),
            store_name: Some(format!("{name}-1.2.3")),
            outputs: vec!["bin".to_string(), "man".to_string()],
            description: Some(format!("the {name} program")),
            licenses: vec!["MIT".to_string()],
            ..Described::default()
        })
        .collect()
}

fn stored(names: &[String]) -> Vec<Stored> {
    names
        .iter()
        .flat_map(|name| {
            ["bin", "man"].map(|output| Stored {
                path: format!("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-{name}-1.2.3-{output}"),
                nar_size: 4096,
                nar_hash: "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
                deriver: Some(format!(
                    "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-{name}.drv"
                )),
                ultimate: false,
                signers: vec!["cache.nixos.org-1".to_string()],
            })
        })
        .collect()
}

#[divan::bench(args = [1, 16, 128])]
fn join_the_facts_of_a_generation(bencher: divan::Bencher, packages: usize) {
    let requested = names(packages);
    let described = described(&requested);
    let stored = stored(&requested);

    bencher.bench_local(|| {
        join(
            divan::black_box(&requested),
            divan::black_box(&described),
            divan::black_box(&stored),
        )
    });
}
