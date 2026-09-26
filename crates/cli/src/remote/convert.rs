use mix_app::bootstrap::{Error as BootstrapError, Host as AppHost};
use mix_app::profile::Error as ActivationError;
use mix_app::repair::RepairReport as AppReport;
use mix_app::target::{Error as TargetError, Unfixable as AppUnfixable};
use mix_rpc::{Failure, Host, RepairReport, TargetFailure, Unfixable};

const NOT_ROOT: &str = "carry out privileged operations";

pub fn failure_from_bootstrap(error: BootstrapError) -> Failure {
    match error {
        BootstrapError::Core(error) => Failure::Core(error),
        BootstrapError::Activation(ActivationError::Core(error)) => Failure::Core(error),
        BootstrapError::Activation(ActivationError::SourceBuildRequired { packages }) => {
            Failure::SourceBuildRequired { packages }
        }
        BootstrapError::Network(error) => Failure::Network(error.to_string()),
        BootstrapError::Integrity { artifact, detail } => Failure::Integrity { artifact, detail },
        BootstrapError::UnsupportedTarget(target) => Failure::UnsupportedTarget(target),
        BootstrapError::Target(error) => Failure::Target(target_failure_from(error)),
        BootstrapError::Decompression(detail) => Failure::Decompression(detail),
        BootstrapError::MalformedArchive(detail) => Failure::MalformedArchive(detail),
        BootstrapError::NotRoot(what) => Failure::NotRoot(what.to_string()),
        BootstrapError::UnsupportedHost => Failure::UnsupportedHost,
        BootstrapError::UnsupportedKernel => Failure::UnsupportedKernel,
        BootstrapError::SystemdNotReady { host } => Failure::SystemdNotReady {
            host: match host {
                AppHost::Native => Host::Native,
                AppHost::Wsl => Host::Wsl,
            },
        },
        BootstrapError::AlreadyManaged => Failure::AlreadyManaged,
        BootstrapError::CrossDeviceStore { path } => Failure::CrossDeviceStore { path },
        BootstrapError::Rollback { cause, summary } => Failure::Rollback {
            cause: Box::new(failure_from_bootstrap(*cause)),
            summary,
        },
        BootstrapError::Interrupted => Failure::Interrupted,
    }
}

pub fn bootstrap_error_from(failure: Failure) -> BootstrapError {
    match failure {
        Failure::Core(error) => BootstrapError::Core(error),
        Failure::SourceBuildRequired { packages } => {
            BootstrapError::Activation(ActivationError::SourceBuildRequired { packages })
        }
        Failure::Network(message) => BootstrapError::Network(message.into()),
        Failure::Integrity { artifact, detail } => BootstrapError::Integrity { artifact, detail },
        Failure::UnsupportedTarget(target) => BootstrapError::UnsupportedTarget(target),
        Failure::Target(failure) => BootstrapError::Target(target_error_from(failure)),
        Failure::Decompression(detail) => BootstrapError::Decompression(detail),
        Failure::MalformedArchive(detail) => BootstrapError::MalformedArchive(detail),
        Failure::NotRoot(_) => BootstrapError::NotRoot(NOT_ROOT),
        Failure::UnsupportedHost => BootstrapError::UnsupportedHost,
        Failure::UnsupportedKernel => BootstrapError::UnsupportedKernel,
        Failure::SystemdNotReady { host } => BootstrapError::SystemdNotReady {
            host: match host {
                Host::Native => AppHost::Native,
                Host::Wsl => AppHost::Wsl,
            },
        },
        Failure::AlreadyManaged => BootstrapError::AlreadyManaged,
        Failure::CrossDeviceStore { path } => BootstrapError::CrossDeviceStore { path },
        Failure::Rollback { cause, summary } => BootstrapError::Rollback {
            cause: Box::new(bootstrap_error_from(*cause)),
            summary,
        },
        Failure::Interrupted => BootstrapError::Interrupted,
    }
}

pub fn target_failure_from(error: TargetError) -> TargetFailure {
    match error {
        TargetError::Core(error) => TargetFailure::Core(error),
        TargetError::Unrepairable { artifact, reason } => TargetFailure::Unrepairable {
            artifact,
            reason: match reason {
                AppUnfixable::NotADirectory => Unfixable::NotADirectory,
                AppUnfixable::MissingUser => Unfixable::MissingUser,
                AppUnfixable::MissingRuntime => Unfixable::MissingRuntime,
            },
        },
    }
}

pub fn target_error_from(failure: TargetFailure) -> TargetError {
    match failure {
        TargetFailure::Core(error) => TargetError::Core(error),
        TargetFailure::Unrepairable { artifact, reason } => TargetError::Unrepairable {
            artifact,
            reason: match reason {
                Unfixable::NotADirectory => AppUnfixable::NotADirectory,
                Unfixable::MissingUser => AppUnfixable::MissingUser,
                Unfixable::MissingRuntime => AppUnfixable::MissingRuntime,
            },
        },
    }
}

pub fn report_to_wire(report: AppReport) -> RepairReport {
    RepairReport {
        name: report.name,
        failure: report.error.map(target_failure_from),
    }
}

pub fn report_from_wire(report: RepairReport) -> AppReport {
    AppReport {
        name: report.name,
        fixed: report.failure.is_none(),
        error: report.failure.map(target_error_from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_bootstrap_error() -> Vec<BootstrapError> {
        vec![
            BootstrapError::Core(mix_core::Error::Locked {
                path: "/var/lib/mix/lock".into(),
            }),
            BootstrapError::Activation(ActivationError::SourceBuildRequired {
                packages: Some(vec!["cowsay-3.8.4".into()]),
            }),
            BootstrapError::Network("connection reset".into()),
            BootstrapError::Integrity {
                artifact: "nix archive".into(),
                detail: "sha256 mismatch".into(),
            },
            BootstrapError::UnsupportedTarget("armv7l-linux".into()),
            BootstrapError::Target(TargetError::Unrepairable {
                artifact: "/nix".into(),
                reason: AppUnfixable::NotADirectory,
            }),
            BootstrapError::Decompression("unexpected end".into()),
            BootstrapError::MalformedArchive("no store".into()),
            BootstrapError::NotRoot(NOT_ROOT),
            BootstrapError::UnsupportedHost,
            BootstrapError::UnsupportedKernel,
            BootstrapError::SystemdNotReady {
                host: AppHost::Native,
            },
            BootstrapError::SystemdNotReady { host: AppHost::Wsl },
            BootstrapError::AlreadyManaged,
            BootstrapError::CrossDeviceStore {
                path: "/nix/store/pkg-a".into(),
            },
            BootstrapError::Rollback {
                cause: Box::new(BootstrapError::Interrupted),
                summary: "1 rollback step(s) failed".into(),
            },
            BootstrapError::Interrupted,
        ]
    }

    #[test]
    fn every_bootstrap_error_reads_the_same_after_crossing_to_the_client() {
        for (original, crossing) in every_bootstrap_error()
            .into_iter()
            .zip(every_bootstrap_error())
        {
            let crossed = bootstrap_error_from(failure_from_bootstrap(crossing));
            assert_eq!(original.to_string(), crossed.to_string());
        }
    }

    #[test]
    fn every_bootstrap_error_is_explained_the_same_after_crossing_to_the_client() {
        let explain = |error: BootstrapError| {
            crate::explain::bootstrap::explain(&anyhow::Error::from(error)).message()
        };
        for (original, crossing) in every_bootstrap_error()
            .into_iter()
            .zip(every_bootstrap_error())
        {
            let crossed = bootstrap_error_from(failure_from_bootstrap(crossing));
            assert_eq!(explain(original), explain(crossed));
        }
    }

    #[test]
    fn a_repair_report_keeps_whether_it_was_fixed() {
        let failed = report_from_wire(report_to_wire(AppReport {
            name: "/nix".into(),
            fixed: false,
            error: Some(TargetError::Unrepairable {
                artifact: "/nix".into(),
                reason: AppUnfixable::MissingRuntime,
            }),
        }));
        assert!(!failed.fixed);
        assert_eq!(
            failed.error.map(|error| error.to_string()),
            Some("/nix: missing, and `mix repair` can't restore it".to_string())
        );

        let fixed = report_from_wire(report_to_wire(AppReport {
            name: "/etc/nix/nix.conf".into(),
            fixed: true,
            error: None,
        }));
        assert!(fixed.fixed);
        assert!(fixed.error.is_none());
    }
}
