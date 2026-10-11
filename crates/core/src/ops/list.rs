use std::collections::{BTreeSet, HashMap};

use mix_events::v1::{InstalledPackage, PackageOutput};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Described {
    pub name: String,
    pub pname: Option<String>,
    pub version: Option<String>,
    pub store_name: Option<String>,
    pub outputs: Vec<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub licenses: Vec<String>,
    pub main_program: Option<String>,
    pub changelog: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub path: String,
    pub nar_size: u64,
    pub nar_hash: String,
    pub deriver: Option<String>,
    pub ultimate: bool,
    pub signers: Vec<String>,
}

pub fn store_name(path: &str) -> Option<&str> {
    let base = path.rsplit('/').next()?;
    base.split_once('-').map(|(_, name)| name)
}

fn expected(store_name: &str, output: &str) -> String {
    if output == "out" {
        store_name.to_string()
    } else {
        format!("{store_name}-{output}")
    }
}

fn outputs_of(described: &Described, stored: &HashMap<&str, &Stored>) -> Vec<PackageOutput> {
    let Some(base) = described.store_name.as_deref() else {
        return Vec::new();
    };
    described
        .outputs
        .iter()
        .filter_map(|output| {
            let found = stored.get(expected(base, output).as_str())?;
            Some(PackageOutput {
                output: output.clone(),
                store_path: found.path.clone(),
                nar_size: found.nar_size,
                nar_hash: found.nar_hash.clone(),
                deriver: found.deriver.clone(),
                built_locally: found.ultimate,
                signers: found.signers.clone(),
            })
        })
        .collect()
}

pub fn join(
    requested: &[String],
    described: &[Described],
    stored: &[Stored],
) -> Vec<InstalledPackage> {
    let names: BTreeSet<&str> = requested.iter().map(String::as_str).collect();
    let mut by_name: HashMap<&str, &Described> = HashMap::with_capacity(described.len());
    for found in described {
        by_name.entry(found.name.as_str()).or_insert(found);
    }
    let mut by_store_name: HashMap<&str, &Stored> = HashMap::with_capacity(stored.len());
    for found in stored {
        if let Some(name) = store_name(&found.path) {
            by_store_name.entry(name).or_insert(found);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let unresolved = InstalledPackage {
                name: name.to_string(),
                ..InstalledPackage::default()
            };
            let Some(found) = by_name.get(name) else {
                return unresolved;
            };
            let outputs = outputs_of(found, &by_store_name);
            if outputs.is_empty() {
                return unresolved;
            }
            InstalledPackage {
                name: name.to_string(),
                pname: found.pname.clone(),
                version: found.version.clone(),
                description: found.description.clone(),
                homepage: found.homepage.clone(),
                licenses: found.licenses.clone(),
                main_program: found.main_program.clone(),
                changelog: found.changelog.clone(),
                outputs,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn described(name: &str, store_name: &str, outputs: &[&str]) -> Described {
        Described {
            name: name.to_string(),
            pname: Some(name.to_string()),
            version: Some("1.0".to_string()),
            store_name: Some(store_name.to_string()),
            outputs: names(outputs),
            description: Some(format!("the {name} program")),
            ..Described::default()
        }
    }

    fn stored(name: &str) -> Stored {
        Stored {
            path: format!("/nix/store/0123456789abcdfghijklmnpqrsvwxyz-{name}"),
            nar_size: name.len() as u64,
            nar_hash: format!("sha256-{name}"),
            deriver: None,
            ultimate: false,
            signers: names(&["cache.nixos.org-1"]),
        }
    }

    #[test]
    fn a_store_name_is_what_follows_the_hash() {
        assert_eq!(
            store_name("/nix/store/msr1v91ybfw6j12rs5mfl8ghb2rqsnsr-git-2.55.0"),
            Some("git-2.55.0")
        );
        assert_eq!(store_name("/nix/store/nodash"), None);
    }

    #[test]
    fn a_package_takes_the_store_path_of_each_output_it_installs() {
        let described = [described("curl", "curl-8.22.0", &["bin", "man"])];
        let stored = [
            stored("curl-8.22.0-man"),
            stored("curl-8.22.0-bin"),
            stored("curl-8.22.0"),
        ];

        let [curl] = &join(&names(&["curl"]), &described, &stored)[..] else {
            panic!("one requested name gives one row");
        };

        assert_eq!(curl.version.as_deref(), Some("1.0"));
        assert_eq!(
            curl.outputs
                .iter()
                .map(|output| (output.output.as_str(), output.store_path.as_str()))
                .collect::<Vec<_>>(),
            [
                (
                    "bin",
                    "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-curl-8.22.0-bin"
                ),
                (
                    "man",
                    "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-curl-8.22.0-man"
                ),
            ],
            "the main output is not installed, so it is not listed, and outputs keep the order they are installed in"
        );
    }

    #[test]
    fn the_default_output_has_the_bare_store_name() {
        let described = [described("tree", "tree-2.3.2", &["out", "man"])];
        let stored = [stored("tree-2.3.2"), stored("tree-2.3.2-man")];

        let rows = join(&names(&["tree"]), &described, &stored);

        assert_eq!(
            rows[0]
                .outputs
                .iter()
                .map(|output| output.output.as_str())
                .collect::<Vec<_>>(),
            ["out", "man"]
        );
    }

    #[test]
    fn what_the_generation_holds_beyond_the_requested_names_is_not_listed() {
        let described = [described("git", "git-2.55.0", &["out"])];
        let stored = [
            stored("git-2.55.0"),
            stored("man-db-2.13.1"),
            stored("hm-session-vars.sh"),
        ];

        let rows = join(&names(&["git"]), &described, &stored);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outputs.len(), 1);
    }

    #[test]
    fn a_name_the_attribute_set_does_not_know_keeps_its_row_without_a_version() {
        let rows = join(&names(&["mystery"]), &[], &[stored("mystery-1.0")]);

        assert_eq!(
            rows,
            [InstalledPackage {
                name: "mystery".to_string(),
                ..InstalledPackage::default()
            }]
        );
    }

    #[test]
    fn a_package_built_from_another_pin_is_not_given_this_pins_facts() {
        let described = [described("git", "git-2.60.0", &["out"])];

        let rows = join(&names(&["git"]), &described, &[stored("git-2.55.0")]);

        assert!(rows[0].outputs.is_empty());
        assert_eq!(rows[0].version, None);
        assert_eq!(rows[0].description, None);
    }

    #[test]
    fn a_described_name_with_no_store_name_is_unresolved() {
        let mut lib = described("lib", "lib", &["out"]);
        lib.store_name = None;

        let rows = join(&names(&["lib"]), &[lib], &[stored("lib")]);

        assert!(rows[0].outputs.is_empty());
    }

    #[test]
    fn rows_are_sorted_by_name_and_a_repeated_name_gives_one() {
        let described = [
            described("zstd", "zstd-1.5.7", &["out"]),
            described("bat", "bat-0.26", &["out"]),
        ];
        let stored = [stored("zstd-1.5.7"), stored("bat-0.26")];

        let rows = join(&names(&["zstd", "bat", "zstd"]), &described, &stored);

        assert_eq!(
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
            ["bat", "zstd"]
        );
    }

    #[test]
    fn the_facts_of_a_store_path_travel_with_its_output() {
        let described = [described("hm", "hm-1.0", &["out"])];
        let mut path = stored("hm-1.0");
        path.ultimate = true;
        path.deriver = Some("/nix/store/aaa-hm-1.0.drv".to_string());

        let rows = join(&names(&["hm"]), &described, &[path]);

        let output = &rows[0].outputs[0];
        assert!(output.built_locally);
        assert_eq!(output.deriver.as_deref(), Some("/nix/store/aaa-hm-1.0.drv"));
        assert_eq!(output.nar_size, "hm-1.0".len() as u64);
        assert_eq!(output.signers, names(&["cache.nixos.org-1"]));
    }

    #[test]
    fn no_requested_names_give_no_rows() {
        assert!(join(&[], &[described("git", "git-2.55.0", &["out"])], &[]).is_empty());
    }
}
