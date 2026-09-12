use crate::constants::NIXBLD_GROUP;
use crate::manifest::{MANIFEST, ManagedArtifact};
use crate::steps::create_users_and_groups::all_users_valid;

const REINSTALL_HINT: &str = "reinstall the managed runtime to restore this";

pub struct HealthReport {
    pub name: String,
    pub healthy: bool,
    pub detail: Option<String>,
    pub hint: Option<&'static str>,
}

impl HealthReport {
    fn healthy(name: &str) -> Self {
        Self {
            name: name.to_string(),
            healthy: true,
            detail: None,
            hint: None,
        }
    }

    fn unhealthy(name: &str, detail: impl std::fmt::Display, hint: &'static str) -> Self {
        Self {
            name: name.to_string(),
            healthy: false,
            detail: Some(detail.to_string()),
            hint: Some(hint),
        }
    }
}

pub async fn audit() -> Vec<HealthReport> {
    let mut reports = Vec::with_capacity(MANIFEST.len() + 1);

    for artifact in MANIFEST {
        reports.push(audit_artifact(artifact).await);
    }

    reports.push(audit_build_users());

    reports
}

async fn audit_artifact(artifact: &ManagedArtifact) -> HealthReport {
    match artifact.check().await {
        Ok(()) => HealthReport::healthy(artifact.label()),
        Err(e) => HealthReport::unhealthy(artifact.label(), e, REINSTALL_HINT),
    }
}

fn audit_build_users() -> HealthReport {
    if all_users_valid() {
        HealthReport::healthy(NIXBLD_GROUP)
    } else {
        HealthReport::unhealthy(
            NIXBLD_GROUP,
            "build users are missing, incomplete, or have the wrong uid/gid",
            REINSTALL_HINT,
        )
    }
}
