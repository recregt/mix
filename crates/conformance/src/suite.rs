use std::path::Path;

use mix_core::testkit::Breakage;
use mix_events::v1::command::Request;
use mix_events::v1::{CleanRequest, DoctorRequest, InstallRequest, RemoveRequest, RepairRequest};
use mix_shell::drive::Fault;
use proptest::prelude::*;
use serde_json::Value;

use crate::{Backend, Faulted};

pub const PACKAGES: [&str; 3] = ["hello", "jq", "ripgrep"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Install(usize),
    Remove(usize),
    Clean(bool),
    Repair,
    Damage(usize),
    Faulted(Box<Step>, usize, FaultKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    Fail,
    CrashBefore,
    CrashAfter,
}

impl FaultKind {
    pub fn fault(self) -> Fault {
        match self {
            FaultKind::Fail => Fault::Fail(mix_core::action::Failure::Io {
                path: "/injected".into(),
                kind: std::io::ErrorKind::StorageFull,
            }),
            FaultKind::CrashBefore => Fault::CrashBefore,
            FaultKind::CrashAfter => Fault::CrashAfter,
        }
    }
}

pub fn command() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..PACKAGES.len()).prop_map(Step::Install),
        (0..PACKAGES.len()).prop_map(Step::Remove),
        any::<bool>().prop_map(Step::Clean),
        Just(Step::Repair),
    ]
}

pub fn step() -> impl Strategy<Value = Step> {
    let fault = prop_oneof![
        Just(FaultKind::Fail),
        Just(FaultKind::CrashBefore),
        Just(FaultKind::CrashAfter),
    ];
    prop_oneof![
        3 => command(),
        1 => any::<usize>().prop_map(Step::Damage),
        1 => (command(), 0..12usize, fault)
            .prop_map(|(command, at, kind)| Step::Faulted(Box::new(command), at, kind)),
    ]
}

pub fn install(packages: &[&str]) -> Request {
    Request::Install(InstallRequest {
        packages: packages.iter().map(|name| name.to_string()).collect(),
    })
}

pub fn bootstrap() -> Request {
    Request::Bootstrap(Box::default())
}

pub fn doctor() -> Request {
    Request::Doctor(DoctorRequest {})
}

pub fn repair() -> Request {
    Request::Repair(RepairRequest {})
}

pub fn request_of(step: &Step) -> Option<(Request, bool)> {
    Some(match step {
        Step::Install(package) => (install(&[PACKAGES[*package]]), false),
        Step::Remove(package) => (
            Request::Remove(RemoveRequest {
                packages: vec![PACKAGES[*package].to_string()],
            }),
            false,
        ),
        Step::Clean(all) => (Request::Clean(CleanRequest { all: *all }), false),
        Step::Repair => (repair(), true),
        Step::Damage(_) | Step::Faulted(..) => return None,
    })
}

pub fn succeeded(document: &Value) -> bool {
    document["status"] == "STATUS_SUCCEEDED"
}

pub fn warned(document: &Value, code: &str) -> bool {
    document["warnings"]
        .as_array()
        .is_some_and(|warnings| warnings.iter().any(|warning| warning["code"] == code))
}

pub fn changes(document: &Value) -> Vec<String> {
    document["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|change| change["status"] == "STATUS_SUCCEEDED")
        .map(|change| {
            format!(
                "{} {} {}",
                change["step"], change["operation"], change["subject"]
            )
        })
        .collect()
}

fn reports(doctor: &Value) -> impl Iterator<Item = &Value> {
    doctor["doctor"]["reports"].as_array().into_iter().flatten()
}

pub fn findings(doctor: &Value) -> Vec<&Value> {
    reports(doctor)
        .filter(|report| report.get("finding").is_some())
        .collect()
}

pub fn fixable(doctor: &Value) -> Vec<String> {
    let refused: Vec<String> = reports(doctor)
        .filter(|report| report.get("unfixable").is_some())
        .filter_map(|report| report["target"].as_str().map(str::to_string))
        .collect();
    reports(doctor)
        .filter(|report| report.get("finding").is_some() && report.get("unfixable").is_none())
        .filter(|report| {
            let target = report["target"].as_str().unwrap_or_default();
            !refused
                .iter()
                .any(|parent| Path::new(target).starts_with(parent))
        })
        .map(|report| format!("{} {}", report["target"], report["finding"]))
        .collect()
}

pub fn unfixable(doctor: &Value) -> bool {
    reports(doctor).any(|report| report.get("unfixable").is_some())
}

fn unexplained(document: &Value) -> bool {
    document["exit"] != 0 && document["problems"].as_array().is_none_or(Vec::is_empty)
}

pub async fn violations(backend: &mut impl Backend, breakage: &Breakage) -> Vec<String> {
    if !backend.damage(breakage).await {
        return Vec::new();
    }
    let at = format!("{:?} {}", breakage.damage, breakage.path.display());
    let mut found = Vec::new();
    let predicted = backend.run(repair(), true, true).await;
    let repaired = backend.run(repair(), false, true).await;
    if changes(&predicted) != changes(&repaired) {
        found.push(format!(
            "{at}: the dry run predicted {:?} but repair made {:?}",
            changes(&predicted),
            changes(&repaired)
        ));
    }
    if unexplained(&repaired) {
        found.push(format!(
            "{at}: repair exited {} without naming a problem",
            repaired["exit"]
        ));
    }
    let doctor = backend.run(doctor(), false, false).await;
    let left = fixable(&doctor);
    if !left.is_empty() {
        found.push(format!("{at}: repair left {left:?}"));
    }
    if !unfixable(&doctor) && !backend.observe().await.repository_records_the_config() {
        found.push(format!("{at}: the repository does not record the config"));
    }
    let again = backend.run(repair(), false, true).await;
    if !changes(&again).is_empty() {
        found.push(format!(
            "{at}: a second repair changed {:?}",
            changes(&again)
        ));
    }
    found
}

pub async fn walk(
    backend: &mut impl Backend,
    breakages: &[Breakage],
    steps: &[Step],
) -> Vec<String> {
    let mut found = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let at = format!("step {index} {step:?} of {steps:?}");
        if let Step::Faulted(command, at_action, kind) = step {
            let (request, as_root) = request_of(command).expect("a faulted step is a command");
            let before = backend.observe().await;
            let done = match backend.faulted(request, as_root, *at_action, *kind).await {
                Faulted::Ended(done) => done,
                Faulted::Crashed | Faulted::Unsupported => continue,
            };
            let atomic = matches!(**command, Step::Install(_) | Step::Remove(_));
            if atomic && !succeeded(&done) && before.journals.is_empty() {
                let mut after = backend.observe().await;
                let kept = !after.journals.is_empty();
                after.journals.clear();
                let incomplete = warned(&done, "CODE_ROLLBACK_INCOMPLETE");
                if kept != incomplete {
                    found.push(format!(
                        "{at}: kept a journal {kept} but said the rollback was incomplete {incomplete}"
                    ));
                }
                if !kept && after != before {
                    found.push(format!("{at}: a failed change was not rolled back"));
                }
            }
            if unexplained(&done) {
                found.push(format!("{at}: exited {} without a problem", done["exit"]));
            }
            continue;
        }
        let Some((request, as_root)) = request_of(step) else {
            if let Step::Damage(pick) = step
                && !breakages.is_empty()
            {
                backend.damage(&breakages[pick % breakages.len()]).await;
            }
            continue;
        };
        let predicted = backend.run(request.clone(), true, as_root).await;
        let done = backend.run(request, false, as_root).await;
        if changes(&predicted) != changes(&done) {
            found.push(format!(
                "{at}: predicted {:?} but made {:?}",
                changes(&predicted),
                changes(&done)
            ));
        }
        if unexplained(&done) {
            found.push(format!("{at}: exited {} without a problem", done["exit"]));
        }
        let seen = backend.observe().await;
        if succeeded(&done) && !seen.journals.is_empty() {
            found.push(format!("{at}: journals left behind {:?}", seen.journals));
        }
        let leftovers = seen.leftovers();
        if !leftovers.is_empty() {
            found.push(format!("{at}: left {leftovers:?}"));
        }
        let changed_packages = matches!(step, Step::Install(_) | Step::Remove(_));
        if changed_packages && succeeded(&done) && seen.list != seen.active {
            found.push(format!("{at}: the list differs from the active generation"));
        }
        if matches!(step, Step::Repair) {
            let doctor = backend.run(doctor(), false, false).await;
            let left = fixable(&doctor);
            if !left.is_empty() {
                found.push(format!("{at}: repair left {left:?}"));
            }
            let again = backend.run(repair(), false, true).await;
            if !changes(&again).is_empty() {
                found.push(format!(
                    "{at}: a second repair changed {:?}",
                    changes(&again)
                ));
            }
        }
    }
    let finale = backend.run(repair(), false, true).await;
    let seen = backend.observe().await;
    if !seen.journals.is_empty() {
        found.push(format!(
            "after {steps:?}: a final repair left journals {:?}: {finale:#}",
            seen.journals
        ));
    }
    let doctor = backend.run(doctor(), false, false).await;
    let left = fixable(&doctor);
    if !left.is_empty() {
        found.push(format!("after {steps:?}: a final repair left {left:?}"));
    }
    found
}
