//! What `mix doctor` says: about a single unhealthy check, and about the audit as a whole.

use mix_app::doctor::HealthReport;

use super::Diagnostic;

/// How the command is spelled when the reader is told to run it again.
const COMMAND: &str = "mix doctor";

pub fn explain(error: &anyhow::Error) -> Diagnostic {
    match error.downcast_ref::<mix_core::Error>() {
        Some(error) => super::core_error(error, COMMAND),
        None => Diagnostic::new(error.to_string()),
    }
}

/// One line per check: the artifact, and what is wrong with it.
///
/// The audit reports facts — a mode, an owner, a missing unit file — and they are printed as
/// they were measured. Only the name keeps its own spelling; nothing is added to it.
pub fn check(report: &HealthReport) -> String {
    match &report.detail {
        Some(detail) => format!("{}: {detail}", report.name),
        None => format!("{}: unhealthy", report.name),
    }
}

/// The verdict at the end of an audit that found something.
pub fn unhealthy() -> Diagnostic {
    Diagnostic::hinting(
        "system health check failed",
        "Run `mix repair` to reconcile configuration drift",
    )
}

/// The verdict when another command refuses to run on an unhealthy system.
///
/// The reader did not ask for an audit, so the check that failed is named where `mix doctor`
/// would have printed it, and they are told where to look.
pub fn blocked(report: &HealthReport) -> Diagnostic {
    Diagnostic::hinting(
        format!("system health check failed: {}", check(report)),
        "Run `mix doctor` to see what drifted, and `mix repair` to reconcile it",
    )
}

#[cfg(test)]
mod tests {
    use mix_core::Category;

    use super::*;

    fn report(name: &str, detail: Option<&str>) -> HealthReport {
        HealthReport {
            name: name.to_string(),
            category: Category::Filesystem,
            healthy: detail.is_none(),
            detail: detail.map(str::to_string),
        }
    }

    #[test]
    fn a_failed_check_keeps_the_name_and_the_measurement() {
        assert_eq!(
            check(&report("/nix", Some("mode is 700, expected 755"))),
            "/nix: mode is 700, expected 755"
        );
    }

    #[test]
    fn a_check_with_nothing_to_say_still_says_it_failed() {
        assert_eq!(check(&report("nixbld1", None)), "nixbld1: unhealthy");
    }

    #[test]
    fn a_blocked_command_names_the_check_and_where_to_look() {
        let message = blocked(&report("/nix", Some("mode is 700, expected 755"))).message();

        assert!(message.contains("/nix: mode is 700, expected 755"));
        assert!(message.contains("mix doctor"));
        assert!(message.contains("mix repair"));
    }
}
