use std::collections::BTreeMap;
use std::path::Path;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::{
    DEFAULT_PROFILE_NIX, DEFAULT_PROFILE_NIX_STORE, GENERATION_STATE_FILE,
};
use mix_core::declared::state::StateManifest;
use mix_core::effect::FlakeSource;
use mix_core::ops::list::{Described, Stored, join};
use mix_core::{Error, Result};
use mix_events::v1::InstalledPackage;
use mix_exec::Scope;
use mix_nixgen::{AttrPath, Installable};
use serde::Deserialize;

use crate::effect::exec::run_as;
use crate::profile::activation;

const HOME_PATH: &str = "home-path";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Facts {
    name: String,
    pname: Option<String>,
    version: Option<String>,
    store_name: Option<String>,
    #[serde(default)]
    outputs: Vec<String>,
    description: Option<String>,
    homepage: Option<String>,
    #[serde(default)]
    licenses: Vec<String>,
    main_program: Option<String>,
    changelog: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    nar_hash: String,
    nar_size: u64,
    deriver: Option<String>,
    #[serde(default)]
    signatures: Vec<String>,
    #[serde(default)]
    ultimate: bool,
}

pub fn parse_facts(text: &str) -> serde_json::Result<Vec<Described>> {
    Ok(serde_json::from_str::<Vec<Facts>>(text)?
        .into_iter()
        .map(|facts| Described {
            name: facts.name,
            pname: facts.pname,
            version: facts.version,
            store_name: facts.store_name,
            outputs: facts.outputs,
            description: facts.description,
            homepage: facts.homepage,
            licenses: facts.licenses,
            main_program: facts.main_program,
            changelog: facts.changelog,
        })
        .collect())
}

fn signer(signature: &str) -> &str {
    signature.split(':').next().unwrap_or(signature)
}

pub fn parse_path_info(text: &str) -> serde_json::Result<Vec<Stored>> {
    Ok(
        serde_json::from_str::<BTreeMap<String, Option<Info>>>(text)?
            .into_iter()
            .filter_map(|(path, info)| {
                let info = info?;
                Some(Stored {
                    path,
                    nar_size: info.nar_size,
                    nar_hash: info.nar_hash,
                    deriver: info.deriver,
                    ultimate: info.ultimate,
                    signers: info
                        .signatures
                        .iter()
                        .map(|signature| signer(signature).to_string())
                        .collect(),
                })
            })
            .collect(),
    )
}

fn unreadable(command: &str, error: impl std::fmt::Display) -> Error {
    Error::Command {
        command: command.to_string(),
        detail: format!("its answer could not be read: {error}"),
    }
}

fn requested(generation: &Path) -> Result<Vec<String>> {
    let path = generation.join(GENERATION_STATE_FILE);
    let text = std::fs::read_to_string(&path).map_err(|source| Error::Io {
        path: path.clone(),
        source,
    })?;
    StateManifest::parse(&text)
        .map(|manifest| manifest.packages)
        .map_err(|error| Error::Io {
            path,
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        })
}

async fn described(
    user: &InvokingUser,
    mirror: Option<&str>,
    names: &[String],
    scope: &Scope,
) -> Result<Vec<Described>> {
    let installable = Installable::new(
        activation::flake(user, FlakeSource::Git),
        AttrPath::new(["homeConfigurations", &user.name, "pkgs"])
            .expect("an invoking user's name never holds a quote"),
    )
    .render();
    let apply = mix_nixgen::package_facts(names.iter().map(String::as_str))
        .map_err(|error| unreadable("nix eval", error))?;
    let mut args = vec![
        "eval".to_string(),
        "--json".to_string(),
        installable,
        "--apply".to_string(),
        apply,
        "--no-write-lock-file".to_string(),
    ];
    args.extend(activation::nix_options(mirror));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    parse_facts(&answer).map_err(|error| unreadable("nix eval", error))
}

async fn stored(user: &InvokingUser, home_path: &Path, scope: &Scope) -> Result<Vec<Stored>> {
    let home_path = home_path.to_string_lossy();
    let listed = run_as(
        user,
        DEFAULT_PROFILE_NIX_STORE,
        &["-q", "--references", &home_path],
        scope,
    )
    .await?;
    let paths: Vec<&str> = listed.lines().filter(|line| !line.is_empty()).collect();
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["path-info", "--json", "--json-format", "1"];
    args.extend(paths);
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    parse_path_info(&answer).map_err(|error| unreadable("nix path-info", error))
}

pub async fn gather(
    user: &InvokingUser,
    generation: &Path,
    mirror: Option<&str>,
    scope: &Scope,
) -> Result<Vec<InstalledPackage>> {
    let names = requested(generation)?;
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let link = generation.join(HOME_PATH);
    let home_path =
        std::fs::canonicalize(&link).map_err(|source| Error::Io { path: link, source })?;
    let (described, stored) = tokio::join!(
        described(user, mirror, &names, scope),
        stored(user, &home_path, scope)
    );
    Ok(join(&names, &described?, &stored?))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    const EVALUATED: &str = r#"[
      {"changelog":"https://curl.se/ch/8.22.0.html","description":"Command line tool for transferring files with URL syntax","homepage":"https://curl.se/","licenses":["curl"],"mainProgram":"curl","name":"curl","outputs":["bin","man"],"pname":"curl","storeName":"curl-8.22.0","version":"8.22.0"},
      {"name":"lib"},
      {"description":"Utility","homepage":null,"licenses":["Unlicense","MIT"],"name":"ripgrep","outputs":["out"],"pname":"ripgrep","storeName":"ripgrep-15.2.0","version":"15.2.0"}
    ]"#;

    const PATH_INFO: &str = r#"{
      "/nix/store/cp8qnyl8i0s62g3a1465i258mf5bcr6k-curl-8.22.0-bin": {
        "ca": null,
        "deriver": "/nix/store/08ps25i5iv3cynwwh6xg9g9smf1fyckg-curl-8.22.0.drv",
        "narHash": "sha256-kDEB+Qan16c9hfC5r9Qn9P2qEcIPT6hzt3R2COHxPzQ=",
        "narSize": 383552,
        "references": ["/nix/store/s21axjr25zw6x9g6ripn7kamgbvw0y69-curl-8.22.0"],
        "registrationTime": 1791478080,
        "signatures": ["cache.nixos.org-1:D1M9Q8x1T7fzaq5gjw7w5WcTP5E5uaJiCLKUPfHZVTNTsxqFClOPuiP5K7dROloN4EpsFpN11qW8FfkVcnSpCQ=="],
        "storeDir": "/nix/store",
        "ultimate": false,
        "version": 1
      },
      "/nix/store/2y30h9ymb8l9ixj5lfl04bfjbk2gvn44-home-configuration-reference-manpage": {
        "ca": null,
        "deriver": "/nix/store/579xvv2zlrsaxcsxapb15av5p82g34c8-home-configuration-reference-manpage.drv",
        "narHash": "sha256-EpQjmldIoNdF8T77aLaOi6+wao+amFpwgENFog+EVVs=",
        "narSize": 2482920,
        "references": [],
        "registrationTime": 1791478189,
        "signatures": [],
        "storeDir": "/nix/store",
        "ultimate": true,
        "version": 1
      },
      "/nix/store/zzzz-not-valid": null
    }"#;

    #[test]
    fn what_nix_eval_answers_becomes_one_description_per_name() {
        let found = parse_facts(EVALUATED).unwrap();

        assert_eq!(found.len(), 3);
        assert_eq!(found[0].name, "curl");
        assert_eq!(found[0].store_name.as_deref(), Some("curl-8.22.0"));
        assert_eq!(found[0].outputs, ["bin", "man"]);
        assert_eq!(
            found[0].changelog.as_deref(),
            Some("https://curl.se/ch/8.22.0.html")
        );
        assert_eq!(found[2].licenses, ["Unlicense", "MIT"]);
        assert_eq!(found[2].homepage, None);
    }

    #[test]
    fn a_name_nix_could_not_describe_arrives_with_nothing_but_its_name() {
        let found = parse_facts(EVALUATED).unwrap();

        assert_eq!(
            found[1],
            Described {
                name: "lib".to_string(),
                ..Described::default()
            }
        );
    }

    #[test]
    fn what_nix_path_info_answers_becomes_one_stored_path_each_in_path_order() {
        let found = parse_path_info(PATH_INFO).unwrap();

        assert_eq!(found.len(), 2);
        assert!(
            found[0]
                .path
                .ends_with("home-configuration-reference-manpage")
        );
        assert!(found[0].ultimate);
        assert!(found[0].signers.is_empty());
        assert_eq!(found[1].nar_size, 383_552);
        assert_eq!(
            found[1].nar_hash,
            "sha256-kDEB+Qan16c9hfC5r9Qn9P2qEcIPT6hzt3R2COHxPzQ="
        );
        assert_eq!(found[1].signers, ["cache.nixos.org-1"]);
        assert_eq!(
            found[1].deriver.as_deref(),
            Some("/nix/store/08ps25i5iv3cynwwh6xg9g9smf1fyckg-curl-8.22.0.drv")
        );
    }

    #[test]
    fn an_answer_that_is_not_what_nix_prints_is_an_error_not_an_empty_list() {
        assert!(parse_facts("not json").is_err());
        assert!(parse_facts(r#"{"name":"x"}"#).is_err());
        assert!(parse_path_info("[]").is_err());
    }

    #[test]
    fn a_signature_names_its_signer_by_what_precedes_the_first_colon() {
        assert_eq!(signer("cache.nixos.org-1:abc+/=="), "cache.nixos.org-1");
        assert_eq!(signer("no-colon"), "no-colon");
    }

    #[test]
    fn the_requested_names_are_the_ones_the_generation_was_built_from() {
        let generation = tempfile::tempdir().unwrap();
        std::fs::write(
            generation.path().join(GENERATION_STATE_FILE),
            StateManifest::seed().render(),
        )
        .unwrap();

        assert_eq!(
            requested(generation.path()).unwrap(),
            StateManifest::seed().packages
        );
    }

    #[test]
    fn a_generation_without_its_package_list_is_an_io_error_naming_the_file() {
        let generation = tempfile::tempdir().unwrap();

        match requested(generation.path()) {
            Err(Error::Io { path, source }) => {
                assert_eq!(path, generation.path().join(GENERATION_STATE_FILE));
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected an I/O error, got {other:?}"),
        }
    }

    #[test]
    fn a_package_list_that_is_not_json_is_invalid_data() {
        let generation = tempfile::tempdir().unwrap();
        std::fs::write(generation.path().join(GENERATION_STATE_FILE), "{broken").unwrap();

        match requested(generation.path()) {
            Err(Error::Io { source, .. }) => {
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
            }
            other => panic!("expected an I/O error, got {other:?}"),
        }
    }
}
