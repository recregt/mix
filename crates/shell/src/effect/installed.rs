use std::collections::BTreeMap;
use std::path::Path;

use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::{
    DEFAULT_PROFILE_NIX, DEFAULT_PROFILE_NIX_STORE, REPOSITORY_BRANCH,
};
use mix_core::effect::FlakeSource;
use mix_core::ops::list::{Described, Stored, join};
use mix_core::ops::update::Versions;
use mix_core::{Error, Result};
use mix_events::v1::{CheckedPackage, FetchPlan, InstalledPackage, PackageVerdict, PlannedOutput};
use mix_exec::Scope;
use mix_home::Active;
use mix_nixgen::lock::LockedInput;
use mix_nixgen::{AttrPath, GitAt, INPUTS, Installable, Pins, Rev};
use serde::Deserialize;

use crate::effect::exec::run_as;

use crate::profile::activation;
use crate::request::upstream::Assessment;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locked {
    pub rev: Rev,
    pub pins: Pins<LockedInput>,
}

fn parse_locked(metadata: &str) -> Option<Locked> {
    let metadata: serde_json::Value = serde_json::from_str(metadata).ok()?;
    Some(Locked {
        rev: Rev::parse(metadata.get("locked")?.get("rev")?.as_str()?).ok()?,
        pins: mix_core::declared::pins::from_lock(metadata.get("locks")?)?,
    })
}

pub async fn locked(user: &InvokingUser, scope: &Scope) -> Result<Option<Locked>> {
    let flake = activation::flake_at(user, GitAt::Branch(REPOSITORY_BRANCH)).render();
    let args = [
        "flake",
        "metadata",
        "--json",
        "--no-update-lock-file",
        &flake,
    ];
    match run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await {
        Ok(answer) => Ok(parse_locked(&answer)),
        Err(cancelled @ Error::Cancelled { .. }) => Err(cancelled),
        Err(_) => Ok(None),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Evaluation<'a> {
    AsItStands,
    AtCommit(Rev),
    Overridden(&'a Pins<LockedInput>),
}

fn evaluation<'a>(wanted: Option<&'a Pins<LockedInput>>, locked: Option<Locked>) -> Evaluation<'a> {
    match (wanted, locked) {
        (None, _) => Evaluation::AsItStands,
        (Some(wanted), Some(locked)) if locked.pins == *wanted => Evaluation::AtCommit(locked.rev),
        (Some(wanted), _) => Evaluation::Overridden(wanted),
    }
}

async fn described(
    user: &InvokingUser,
    mirror: Option<&str>,
    names: &[String],
    pins: Option<&Pins<LockedInput>>,
    scope: &Scope,
) -> Result<Vec<Described>> {
    let locked = match pins {
        Some(_) => locked(user, scope).await?,
        None => None,
    };
    let apply = mix_nixgen::package_facts(names.iter().map(String::as_str))
        .map_err(|error| unreadable("nix eval", error))?;
    let args = eval_args(user, apply, &evaluation(pins, locked), mirror);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    parse_facts(&answer).map_err(|error| unreadable("nix eval", error))
}

fn eval_args(
    user: &InvokingUser,
    apply: String,
    evaluation: &Evaluation,
    mirror: Option<&str>,
) -> Vec<String> {
    let flake = match evaluation {
        Evaluation::AtCommit(rev) => activation::flake_at(user, GitAt::Commit(rev.clone())),
        Evaluation::AsItStands | Evaluation::Overridden(_) => {
            activation::flake(user, FlakeSource::Git)
        }
    };
    let installable = Installable::new(
        flake,
        AttrPath::new(["homeConfigurations", &user.name, "pkgs"])
            .expect("an invoking user's name never holds a quote"),
    )
    .render();
    let mut args = vec![
        "eval".to_string(),
        "--json".to_string(),
        installable,
        "--apply".to_string(),
        apply,
        "--no-write-lock-file".to_string(),
    ];
    if let Evaluation::Overridden(pins) = evaluation {
        args.extend(overrides(pins));
    }
    args.extend(activation::nix_options(mirror));
    args
}

fn evaluated_at(generation: &Active, mirror: Option<&str>) -> Option<Pins<LockedInput>> {
    match crate::effect::mirror::filter_mirror(mirror) {
        Some(_) => None,
        None => crate::effect::generations::pins(generation),
    }
}

fn overrides(pins: &Pins<LockedInput>) -> Vec<String> {
    INPUTS
        .iter()
        .flat_map(|input| {
            let pin = pins.of(input.pin);
            [
                "--override-input".to_string(),
                input.name.to_string(),
                format!(
                    "github:{}/{}/{}?narHash={}",
                    input.owner,
                    input.repo,
                    pin.rev.as_str(),
                    crate::effect::mirror::query_value(pin.nar_hash.as_str())
                ),
            ]
        })
        .collect()
}

pub async fn versions_at(
    user: &InvokingUser,
    names: &[String],
    pins: &Pins<LockedInput>,
    scope: &Scope,
) -> Result<Versions> {
    if names.is_empty() {
        return Ok(Versions::new());
    }
    Ok(described(user, None, names, Some(pins), scope)
        .await?
        .into_iter()
        .map(|described| (described.name, described.version))
        .collect())
}

pub async fn evaluate(
    user: &InvokingUser,
    names: &[String],
    mirror: Option<&str>,
    scope: &Scope,
) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    let args = evaluate_args(user, names, mirror)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_as(user, DEFAULT_PROFILE_NIX, &args, scope)
        .await
        .map(|_| ())
}

#[derive(Deserialize)]
struct Assessed {
    facts: Vec<Facts>,
    checks: Vec<Check>,
}

#[derive(Deserialize)]
struct Check {
    name: String,
    known: bool,
    #[serde(default)]
    evaluates: bool,
    #[serde(default)]
    unfree: bool,
    #[serde(default)]
    broken: bool,
    #[serde(default)]
    insecure: bool,
    derivation: Option<String>,
    #[serde(default)]
    outputs: Vec<CheckedOutput>,
}

#[derive(Deserialize)]
struct CheckedOutput {
    output: String,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Narinfo {
    nar_size: u64,
    download_size: Option<u64>,
}

fn verdict(check: &Check) -> PackageVerdict {
    match check {
        Check { known: false, .. } => PackageVerdict::Unknown,
        Check {
            evaluates: true, ..
        } => PackageVerdict::Available,
        Check { unfree: true, .. } => PackageVerdict::Unfree,
        Check { broken: true, .. } => PackageVerdict::Broken,
        Check { insecure: true, .. } => PackageVerdict::Insecure,
        _ => PackageVerdict::Refused,
    }
}

pub fn parse_assessment(text: &str) -> serde_json::Result<Vec<CheckedPackage>> {
    let assessed: Assessed = serde_json::from_str(text)?;
    let facts: BTreeMap<&str, &Facts> = assessed
        .facts
        .iter()
        .map(|facts| (facts.name.as_str(), facts))
        .collect();
    Ok(assessed
        .checks
        .iter()
        .map(|check| {
            let found = facts.get(check.name.as_str());
            let fact = |pick: fn(&Facts) -> &Option<String>| found.and_then(|f| pick(f).clone());
            CheckedPackage {
                name: check.name.clone(),
                verdict: verdict(check) as i32,
                pname: fact(|f| &f.pname),
                version: fact(|f| &f.version),
                description: fact(|f| &f.description),
                homepage: fact(|f| &f.homepage),
                licenses: found.map(|f| f.licenses.clone()).unwrap_or_default(),
                main_program: fact(|f| &f.main_program),
                changelog: fact(|f| &f.changelog),
                derivation: check.derivation.clone(),
                outputs: check
                    .outputs
                    .iter()
                    .map(|output| PlannedOutput {
                        output: output.output.clone(),
                        store_path: output.path.clone(),
                        ..PlannedOutput::default()
                    })
                    .collect(),
            }
        })
        .collect())
}

pub fn parse_narinfo(text: &str) -> serde_json::Result<BTreeMap<String, (u64, u64)>> {
    Ok(
        serde_json::from_str::<BTreeMap<String, Option<Narinfo>>>(text)?
            .into_iter()
            .filter_map(|(path, info)| {
                info.map(|info| (path, (info.nar_size, info.download_size.unwrap_or(0))))
            })
            .collect(),
    )
}

pub async fn assess(
    user: &InvokingUser,
    names: &[String],
    mirror: Option<&str>,
    scope: &Scope,
) -> Result<Assessment> {
    if names.is_empty() {
        return Ok(Assessment::default());
    }
    let apply = mix_nixgen::package_checks(names.iter().map(String::as_str))
        .map_err(|error| unreadable("nix eval", error))?;
    let mut args = eval_args(user, apply, &Evaluation::AsItStands, mirror);
    args.insert(1, "--read-only".to_string());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    let mut packages = parse_assessment(&answer).map_err(|error| unreadable("nix eval", error))?;
    let refused: Vec<String> = packages
        .iter()
        .filter(|package| package.verdict() != PackageVerdict::Available)
        .map(|package| package.name.clone())
        .collect();
    let refusal = match refused.is_empty() {
        true => None,
        false => evaluate(user, &refused, mirror, scope).await.err(),
    };
    let plan = plan(user, &mut packages, scope).await;
    Ok(Assessment {
        packages,
        plan: Some(plan),
        refusal,
    })
}

pub async fn outputs_of(
    user: &InvokingUser,
    source: FlakeSource,
    names: &[String],
    mirror: Option<&str>,
    scope: &Scope,
) -> Result<Vec<CheckedPackage>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let args = outputs_args(user, source, names, mirror)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer = run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await?;
    parse_assessment(&answer).map_err(|error| unreadable("nix eval", error))
}

fn outputs_args(
    user: &InvokingUser,
    source: FlakeSource,
    names: &[String],
    mirror: Option<&str>,
) -> Result<Vec<String>> {
    let apply = mix_nixgen::package_checks(names.iter().map(String::as_str))
        .map_err(|error| unreadable("nix eval", error))?;
    let installable = Installable::new(
        activation::flake(user, source),
        AttrPath::new(["homeConfigurations", &user.name, "pkgs"])
            .expect("an invoking user's name never holds a quote"),
    )
    .render();
    let mut args = vec![
        "eval".to_string(),
        "--read-only".to_string(),
        "--json".to_string(),
        installable,
        "--apply".to_string(),
        apply,
        "--no-write-lock-file".to_string(),
    ];
    args.extend(activation::nix_options(mirror));
    Ok(args)
}

pub fn owner_of(packages: &[CheckedPackage], path: &str) -> String {
    packages
        .iter()
        .find(|package| {
            package
                .outputs
                .iter()
                .any(|output| output.store_path == path)
        })
        .map_or_else(
            || mix_nixlog::package_name(path).to_string(),
            |package| package.name.clone(),
        )
}

enum Fetched {
    From(String, BTreeMap<String, (u64, u64)>),
    Nowhere,
    Unknown,
}

async fn invalid(user: &InvokingUser, paths: &[&str], scope: &Scope) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["--check-validity", "--print-invalid"];
    args.extend(paths);
    let listed = run_as(user, DEFAULT_PROFILE_NIX_STORE, &args, scope).await?;
    Ok(listed.lines().map(str::to_string).collect())
}

async fn fetched_from(
    user: &InvokingUser,
    substituters: &[String],
    path: &str,
    scope: &Scope,
) -> Fetched {
    let mut unknown = false;
    for substituter in substituters {
        let args = [
            "path-info",
            "--store",
            substituter,
            "--recursive",
            "--json",
            "--json-format",
            "1",
            path,
        ];
        match run_as(user, DEFAULT_PROFILE_NIX, &args, scope).await {
            Ok(answer) => match parse_narinfo(&answer) {
                Ok(closure) => return Fetched::From(substituter.clone(), closure),
                Err(_) => unknown = true,
            },
            Err(Error::Command { detail, .. }) if detail.contains("is not valid") => {}
            Err(_) => unknown = true,
        }
    }
    if unknown {
        Fetched::Unknown
    } else {
        Fetched::Nowhere
    }
}

async fn plan(user: &InvokingUser, packages: &mut [CheckedPackage], scope: &Scope) -> FetchPlan {
    let mut plan = FetchPlan {
        complete: true,
        ..FetchPlan::default()
    };
    let outputs: Vec<String> = packages
        .iter()
        .flat_map(|package| {
            package
                .outputs
                .iter()
                .map(|output| output.store_path.clone())
        })
        .collect();
    let paths: Vec<&str> = outputs.iter().map(String::as_str).collect();
    let (Ok(missing), Ok(substituters)) = (
        invalid(user, &paths, scope).await,
        run_as(
            user,
            DEFAULT_PROFILE_NIX,
            &["config", "show", "substituters"],
            scope,
        )
        .await,
    ) else {
        plan.complete = false;
        return plan;
    };
    let substituters: Vec<String> = substituters
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut closure = BTreeMap::new();
    for output in packages
        .iter_mut()
        .flat_map(|package| package.outputs.iter_mut())
    {
        output.present = !missing.contains(&output.store_path);
        if output.present {
            continue;
        }
        match fetched_from(user, &substituters, &output.store_path, scope).await {
            Fetched::From(substituter, found) => {
                if let Some((nar_size, download_size)) = found.get(&output.store_path) {
                    output.nar_size = Some(*nar_size);
                    output.download_size = Some(*download_size);
                }
                output.substituter = Some(substituter);
                closure.extend(found);
            }
            Fetched::Nowhere => plan.built.push(output.store_path.clone()),
            Fetched::Unknown => plan.complete = false,
        }
    }
    let paths: Vec<&str> = closure.keys().map(String::as_str).collect();
    let Ok(needed) = invalid(user, &paths, scope).await else {
        plan.complete = false;
        return plan;
    };
    for path in &needed {
        if let Some((nar_size, download_size)) = closure.get(path) {
            plan.paths += 1;
            plan.unpacked_bytes += nar_size;
            plan.download_bytes += download_size;
        }
    }
    plan
}

fn evaluate_args(
    user: &InvokingUser,
    names: &[String],
    mirror: Option<&str>,
) -> Result<Vec<String>> {
    let apply = mix_nixgen::derivations(names.iter().map(String::as_str))
        .map_err(|error| unreadable("nix eval", error))?;
    let mut args = eval_args(user, apply, &Evaluation::AsItStands, mirror);
    args.insert(1, "--read-only".to_string());
    Ok(args)
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

#[allow(clippy::disallowed_methods)]
pub async fn gather(
    user: &InvokingUser,
    generation: &Active,
    names: &[String],
    mirror: Option<&str>,
    scope: &Scope,
) -> Result<Vec<InstalledPackage>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let pins = evaluated_at(generation, mirror);
    let link = generation.path.join(HOME_PATH);
    let home_path =
        std::fs::canonicalize(&link).map_err(|source| Error::Io { path: link, source })?;
    let (described, stored) = tokio::join!(
        described(user, mirror, names, pins.as_ref(), scope),
        stored(user, &home_path, scope)
    );
    Ok(join(names, &described?, &stored?))
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

    fn built_at(lock: Option<String>) -> Active {
        Active {
            number: 1,
            path: "/nix/store/aaa-home-manager-generation".into(),
            list: None,
            lock,
        }
    }

    fn embedded() -> Active {
        built_at(Some(mix_nixgen::lock::render(mix_nixgen::lock::EMBEDDED)))
    }

    #[test]
    fn a_generation_is_evaluated_at_the_pins_it_was_built_from() {
        assert_eq!(
            evaluated_at(&embedded(), None),
            Some(mix_nixgen::lock::EMBEDDED)
        );
        assert_eq!(
            evaluated_at(&embedded(), Some("  ")),
            Some(mix_nixgen::lock::EMBEDDED)
        );
    }

    #[test]
    fn on_a_mirror_the_mirror_decides_what_the_inputs_are() {
        assert_eq!(
            evaluated_at(&embedded(), Some("http://mirror.internal")),
            None
        );
    }

    #[test]
    fn a_generation_without_a_readable_lock_is_evaluated_from_the_profile_flake() {
        assert_eq!(evaluated_at(&built_at(None), None), None);
        assert_eq!(evaluated_at(&built_at(Some("{".into())), None), None);
    }

    fn user() -> InvokingUser {
        InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".to_string(),
            home: "/home/alice".into(),
        }
    }

    const COMMIT: Rev = Rev::new_static("8f02d2b8397a3e8e5945017b8ca33ee91ef67eb0");

    fn locked_at(pins: Pins<LockedInput>) -> Locked {
        Locked { rev: COMMIT, pins }
    }

    #[test]
    fn the_pins_an_evaluation_holds_override_each_input_with_its_hash() {
        let pins = mix_nixgen::lock::EMBEDDED;
        let args = eval_args(
            &user(),
            "pkgs: pkgs".into(),
            &Evaluation::Overridden(&pins),
            None,
        );

        assert_eq!(
            args[2],
            "git+file:///home/alice/.local/state/mix#homeConfigurations.alice.pkgs"
        );
        for input in &INPUTS {
            let pin = pins.of(input.pin);
            let at = args
                .iter()
                .position(|arg| arg == input.name)
                .expect("each input is overridden");
            assert_eq!(args[at - 1], "--override-input");
            assert!(args[at + 1].starts_with(&format!(
                "github:{}/{}/{}?narHash=",
                input.owner,
                input.repo,
                pin.rev.as_str()
            )));
        }
        assert!(args.iter().any(|arg| arg == "--no-write-lock-file"));
    }

    #[test]
    fn a_mirrored_evaluation_overrides_the_inputs_only_with_the_mirror() {
        let wanted = evaluated_at(&embedded(), Some("http://mirror.internal"));
        let evaluation = evaluation(wanted.as_ref(), Some(locked_at(mix_nixgen::lock::EMBEDDED)));
        assert_eq!(evaluation, Evaluation::AsItStands);
        let args = eval_args(
            &user(),
            "pkgs: pkgs".into(),
            &evaluation,
            Some("http://mirror.internal"),
        );

        assert!(!args.iter().any(|arg| arg.starts_with("github:")));
        assert_eq!(
            args.iter().filter(|arg| *arg == "--override-input").count(),
            INPUTS.len()
        );
        assert!(
            args.iter()
                .any(|arg| arg.starts_with("tarball+http://mirror.internal"))
        );
    }

    const METADATA: &str = include_str!("../../fixtures/nix/flake-metadata.json");

    #[test]
    fn what_nix_flake_metadata_prints_is_read_as_the_commit_and_the_pins_it_locks() {
        assert_eq!(
            parse_locked(METADATA),
            Some(locked_at(mix_nixgen::lock::EMBEDDED))
        );
    }

    #[test]
    fn a_flake_with_uncommitted_changes_has_no_commit_to_evaluate_at() {
        let mut metadata: serde_json::Value = serde_json::from_str(METADATA).unwrap();
        let root = metadata["locked"].as_object_mut().unwrap();
        root.remove("rev");
        root.insert(
            "dirtyRev".to_string(),
            format!("{}-dirty", COMMIT.as_str()).into(),
        );

        assert_eq!(parse_locked(&metadata.to_string()), None);
    }

    #[test]
    fn metadata_without_a_lock_holds_no_pins() {
        assert_eq!(parse_locked("not json"), None);
        let mut metadata: serde_json::Value = serde_json::from_str(METADATA).unwrap();
        metadata.as_object_mut().unwrap().remove("locks");
        assert_eq!(parse_locked(&metadata.to_string()), None);
    }

    #[test]
    fn pins_the_profile_flake_is_committed_at_are_evaluated_at_that_commit() {
        let pins = mix_nixgen::lock::EMBEDDED;
        let evaluation = evaluation(Some(&pins), Some(locked_at(pins.clone())));
        assert_eq!(evaluation, Evaluation::AtCommit(COMMIT));

        let args = eval_args(&user(), "pkgs: pkgs".into(), &evaluation, None);

        assert_eq!(
            args[2],
            format!(
                "git+file:///home/alice/.local/state/mix?rev={}#homeConfigurations.alice.pkgs",
                COMMIT.as_str()
            )
        );
        assert!(!args.iter().any(|arg| arg == "--override-input"));
    }

    #[test]
    fn pins_the_profile_flake_is_not_committed_at_are_overridden() {
        let pins = mix_nixgen::lock::EMBEDDED;
        let mut other = pins.clone();
        other.nixpkgs.rev = pins.home_manager.rev.clone();

        assert_eq!(
            evaluation(Some(&pins), Some(locked_at(other))),
            Evaluation::Overridden(&pins)
        );
        assert_eq!(evaluation(Some(&pins), None), Evaluation::Overridden(&pins));
    }

    #[test]
    fn without_pins_to_hold_the_flake_is_evaluated_as_it_stands() {
        let pins = mix_nixgen::lock::EMBEDDED;
        let evaluation = evaluation(None, Some(locked_at(pins)));
        assert_eq!(evaluation, Evaluation::AsItStands);

        let args = eval_args(&user(), "pkgs: pkgs".into(), &evaluation, None);

        assert_eq!(
            args[2],
            "git+file:///home/alice/.local/state/mix#homeConfigurations.alice.pkgs"
        );
        assert!(!args.iter().any(|arg| arg == "--override-input"));
    }

    #[test]
    fn new_packages_are_evaluated_read_only_in_the_profile_as_it_stands() {
        let names = ["make".to_string(), "jq".to_string()];

        let args = evaluate_args(&user(), &names, None).unwrap();

        assert_eq!(&args[..2], ["eval", "--read-only"]);
        assert_eq!(
            args[3],
            "git+file:///home/alice/.local/state/mix#homeConfigurations.alice.pkgs"
        );
        assert_eq!(
            args[5],
            "pkgs: map (name: pkgs.${name}.drvPath) [\n  \"make\"\n  \"jq\"\n]"
        );
        assert!(!args.iter().any(|arg| arg == "--override-input"));
    }

    #[test]
    fn new_packages_on_a_mirror_are_evaluated_from_it() {
        let args =
            evaluate_args(&user(), &["jq".to_string()], Some("http://mirror.internal")).unwrap();

        assert!(
            args.iter()
                .any(|arg| arg.starts_with("tarball+http://mirror.internal"))
        );
        assert!(!args.iter().any(|arg| arg.starts_with("github:")));
    }

    const ASSESSED: &str = include_str!("../../fixtures/nix/assess.json");
    const CLOSURE: &str = include_str!("../../fixtures/nix/path-info-closure.json");

    #[test]
    fn what_nix_says_about_new_packages_becomes_one_verdict_each_with_its_facts() {
        let packages = parse_assessment(ASSESSED).unwrap();

        let verdicts: Vec<(&str, PackageVerdict)> = packages
            .iter()
            .map(|package| (package.name.as_str(), package.verdict()))
            .collect();
        assert_eq!(
            verdicts,
            [
                ("hello", PackageVerdict::Available),
                ("make", PackageVerdict::Unknown),
                ("unrar", PackageVerdict::Unfree),
                ("python2", PackageVerdict::Refused),
            ]
        );
        let hello = &packages[0];
        assert_eq!(hello.version.as_deref(), Some("2.12.3"));
        assert_eq!(hello.licenses, ["GPL-3.0-or-later"]);
        assert_eq!(
            hello.derivation.as_deref(),
            Some("/nix/store/gdxx5ilpli16l40mg2z9vs4b0w6wa1gf-hello-2.12.3.drv")
        );
        assert_eq!(
            hello
                .outputs
                .iter()
                .map(|output| (output.output.as_str(), output.store_path.as_str()))
                .collect::<Vec<_>>(),
            [(
                "out",
                "/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3"
            )]
        );
        assert_eq!(packages[2].licenses, ["unfreeRedistributable"]);
        assert!(packages[2].derivation.is_none() && packages[2].outputs.is_empty());
        assert!(packages[1].version.is_none() && packages[1].licenses.is_empty());
    }

    #[test]
    fn a_closure_from_the_cache_holds_the_unpacked_and_download_size_of_each_path() {
        let closure = parse_narinfo(CLOSURE).unwrap();

        assert_eq!(closure.len(), 5);
        assert_eq!(
            closure["/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3"],
            (279_624, 75_355)
        );
        assert_eq!(
            closure["/nix/store/lm3pknxi0ipypy3lxh1wmm8wvvavdwrn-glibc-2.42-84"],
            (35_073_128, 10_250_422)
        );
    }

    #[test]
    fn a_store_path_is_named_by_the_package_whose_output_it_is() {
        let packages = parse_assessment(ASSESSED).unwrap();

        assert_eq!(
            owner_of(
                &packages,
                "/nix/store/xl1h9i29pgq2q5cszjhm5wpfxfbbqwyi-hello-2.12.3"
            ),
            "hello"
        );
        assert_eq!(
            owner_of(
                &packages,
                "/nix/store/k7qqryx7cwwyb6gpck0bh56izx9g6bpa-lua-5.3.6"
            ),
            "lua-5.3.6"
        );
    }

    #[test]
    fn the_outputs_of_a_profile_are_evaluated_from_the_flake_its_build_used() {
        let names = ["lua5_3".to_string(), "lua5_4".to_string()];

        let first = outputs_args(&user(), FlakeSource::Path, &names, None).unwrap();
        let mirrored = outputs_args(
            &user(),
            FlakeSource::Git,
            &names,
            Some("http://mirror.internal"),
        )
        .unwrap();

        assert_eq!(&first[..3], ["eval", "--read-only", "--json"]);
        assert_eq!(
            first[3],
            "path:/home/alice/.local/state/mix#homeConfigurations.alice.pkgs"
        );
        assert!(first[5].contains("checks = (pkgs: names:"));
        assert!(!first.iter().any(|arg| arg == "--override-input"));
        assert_eq!(
            mirrored[3],
            "git+file:///home/alice/.local/state/mix#homeConfigurations.alice.pkgs"
        );
        assert!(
            mirrored
                .iter()
                .any(|arg| arg.starts_with("tarball+http://mirror.internal"))
        );
    }

    #[test]
    fn an_answer_that_is_not_an_assessment_is_an_error() {
        assert!(parse_assessment("[]").is_err());
        assert!(parse_narinfo("[]").is_err());
    }

    #[test]
    fn a_signature_names_its_signer_by_what_precedes_the_first_colon() {
        assert_eq!(signer("cache.nixos.org-1:abc+/=="), "cache.nixos.org-1");
        assert_eq!(signer("no-colon"), "no-colon");
    }
}
