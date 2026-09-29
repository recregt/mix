use futures_util::StreamExt;
use mix_core::Scope;
use mix_core::action::{Action, Failure, Outcome, Performed, UnitFacts, UnitFailure};
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

use crate::effect::files::Prepared;

const DESTINATION: &str = "org.freedesktop.systemd1";
const MANAGER_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const UNIT: &str = "org.freedesktop.systemd1.Unit";

pub struct Units {
    bus: Connection,
}

pub fn interface_of(unit: &str) -> Option<&'static str> {
    let kind = unit.rsplit_once('.')?.1;
    Some(match kind {
        "service" => "org.freedesktop.systemd1.Service",
        "socket" => "org.freedesktop.systemd1.Socket",
        "mount" => "org.freedesktop.systemd1.Mount",
        "timer" => "org.freedesktop.systemd1.Timer",
        "swap" => "org.freedesktop.systemd1.Swap",
        _ => return None,
    })
}

pub fn method_failure(unit: &str, name: &str, message: &str) -> Failure {
    match name {
        "org.freedesktop.systemd1.NoSuchUnit" | "org.freedesktop.systemd1.LoadFailed" => {
            Failure::Unit(Box::new(UnitFailure {
                unit: unit.to_string(),
                job_result: "failed".to_string(),
                active_state: "inactive".to_string(),
                sub_state: "dead".to_string(),
                unit_result: "not-found".to_string(),
                invocation: None,
            }))
        }
        "org.freedesktop.DBus.Error.AccessDenied"
        | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => Failure::Io {
            path: unit.into(),
            kind: std::io::ErrorKind::PermissionDenied,
        },
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NoServer"
        | "org.freedesktop.DBus.Error.Disconnected" => Failure::SystemdUnreachable,
        _ => Failure::CommandFailed {
            program: "systemd".to_string(),
            status: None,
            output_tail: format!("{name}: {message}"),
        },
    }
}

fn bus_failure(unit: &str, error: zbus::Error) -> Failure {
    match error {
        zbus::Error::MethodError(name, message, _) => {
            method_failure(unit, name.as_str(), message.as_deref().unwrap_or(""))
        }
        zbus::Error::FDO(error) => {
            let name = format!("org.freedesktop.DBus.Error.{}", fdo_name(&error));
            method_failure(unit, &name, &error.to_string())
        }
        zbus::Error::InputOutput(_) | zbus::Error::Handshake(_) | zbus::Error::Address(_) => {
            Failure::SystemdUnreachable
        }
        other => Failure::CommandFailed {
            program: "systemd".to_string(),
            status: None,
            output_tail: other.to_string(),
        },
    }
}

fn fdo_name(error: &zbus::fdo::Error) -> &'static str {
    match error {
        zbus::fdo::Error::AccessDenied(_) => "AccessDenied",
        zbus::fdo::Error::InteractiveAuthorizationRequired(_) => "InteractiveAuthorizationRequired",
        zbus::fdo::Error::ServiceUnknown(_) => "ServiceUnknown",
        zbus::fdo::Error::NoServer(_) => "NoServer",
        zbus::fdo::Error::Disconnected(_) => "Disconnected",
        _ => "Failed",
    }
}

pub fn invocation(bytes: &[u8]) -> Option<String> {
    (!bytes.is_empty()).then(|| bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn conflict(unit: &str, expected: &str, found: &str) -> Failure {
    Failure::Conflict {
        subject: unit.to_string(),
        expected: expected.to_string(),
        found: found.to_string(),
    }
}

fn done(undo: Vec<Action>) -> Outcome {
    Ok(Performed { undo })
}

impl Units {
    pub async fn connect() -> Result<Self, Failure> {
        Connection::system()
            .await
            .map(|bus| Self { bus })
            .map_err(|_| Failure::SystemdUnreachable)
    }

    async fn manager(&self) -> Result<Proxy<'_>, Failure> {
        Proxy::new(&self.bus, DESTINATION, MANAGER_PATH, MANAGER)
            .await
            .map_err(|error| bus_failure("systemd", error))
    }

    async fn unit_proxy(&self, unit: &str, interface: &'static str) -> Result<Proxy<'_>, Failure> {
        let manager = self.manager().await?;
        let path: OwnedObjectPath = manager
            .call("LoadUnit", &(unit,))
            .await
            .map_err(|error| bus_failure(unit, error))?;
        Proxy::new(&self.bus, DESTINATION, path, interface)
            .await
            .map_err(|error| bus_failure(unit, error))
    }

    pub async fn observe(&self, unit: &str) -> Result<UnitFacts, Failure> {
        let proxy = self.unit_proxy(unit, UNIT).await?;
        let failed = |error| bus_failure(unit, error);
        let load_state: String = proxy.get_property("LoadState").await.map_err(failed)?;
        let active_state: String = proxy.get_property("ActiveState").await.map_err(failed)?;
        let file_state: String = proxy.get_property("UnitFileState").await.map_err(failed)?;
        let needs_reload: bool = proxy
            .get_property("NeedDaemonReload")
            .await
            .map_err(failed)?;
        Ok(UnitFacts {
            load_state,
            active_state,
            enabled: file_state == "enabled",
            needs_reload,
        })
    }

    async fn failure(&self, unit: &str, job_result: &str) -> Failure {
        let (active_state, sub_state, invocation) = match self.unit_proxy(unit, UNIT).await {
            Ok(proxy) => (
                proxy
                    .get_property::<String>("ActiveState")
                    .await
                    .unwrap_or_default(),
                proxy
                    .get_property::<String>("SubState")
                    .await
                    .unwrap_or_default(),
                proxy
                    .get_property::<Vec<u8>>("InvocationID")
                    .await
                    .ok()
                    .and_then(|bytes| invocation(&bytes)),
            ),
            Err(_) => Default::default(),
        };
        let unit_result = match interface_of(unit) {
            Some(interface) => match self.unit_proxy(unit, interface).await {
                Ok(proxy) => proxy
                    .get_property::<String>("Result")
                    .await
                    .unwrap_or_default(),
                Err(_) => String::new(),
            },
            None => String::new(),
        };
        Failure::Unit(Box::new(UnitFailure {
            unit: unit.to_string(),
            job_result: job_result.to_string(),
            active_state,
            sub_state,
            unit_result,
            invocation,
        }))
    }

    async fn job(&self, method: &str, unit: &str, scope: &Scope) -> Result<(), Failure> {
        let manager = self.manager().await?;
        let failed = |error| bus_failure(unit, error);
        let _ = manager.call::<_, _, ()>("Subscribe", &()).await;
        let mut removed = manager.receive_signal("JobRemoved").await.map_err(failed)?;
        let job: OwnedObjectPath = manager
            .call(method, &(unit, "replace"))
            .await
            .map_err(failed)?;
        let result = scope
            .guard(async {
                while let Some(signal) = removed.next().await {
                    let Ok((_, path, _, result)) =
                        signal
                            .body()
                            .deserialize::<(u32, OwnedObjectPath, String, String)>()
                    else {
                        continue;
                    };
                    if path == job {
                        return Some(result);
                    }
                }
                None
            })
            .await
            .map_err(|_| Failure::Cancelled)?;
        match result.as_deref() {
            Some("done") => Ok(()),
            Some(result) => Err(self.failure(unit, result).await),
            None => Err(Failure::SystemdUnreachable),
        }
    }

    pub async fn perform(
        &self,
        action: &Action,
        scope: &Scope,
        prepared: &mut Prepared<'_>,
    ) -> Option<Outcome> {
        Some(match action {
            Action::DaemonReload => self.reload(prepared).await,
            Action::EnableUnit { unit } => self.enable(unit, prepared).await,
            Action::DisableUnit { unit } => self.disable(unit, prepared).await,
            Action::StartUnit { unit } => self.start(unit, scope, prepared).await,
            Action::StopUnit { unit } => self.stop(unit, scope, prepared).await,
            Action::RestartUnit { unit } => {
                let undo = vec![Action::RestartUnit { unit: unit.clone() }];
                match prepared(&undo) {
                    Err(failure) => Err(failure),
                    Ok(()) => self
                        .job("TryRestartUnit", unit, scope)
                        .await
                        .and_then(|()| done(undo)),
                }
            }
            _ => return None,
        })
    }

    async fn reload(&self, prepared: &mut Prepared<'_>) -> Outcome {
        prepared(&[Action::DaemonReload])?;
        self.manager()
            .await?
            .call::<_, _, ()>("Reload", &())
            .await
            .map_err(|error| bus_failure("systemd", error))?;
        done(vec![Action::DaemonReload])
    }

    async fn enable(&self, unit: &str, prepared: &mut Prepared<'_>) -> Outcome {
        let facts = self.observe(unit).await?;
        if facts.enabled {
            return Err(conflict(unit, "disabled", "enabled"));
        }
        prepared(&[Action::DisableUnit {
            unit: unit.to_string(),
        }])?;
        let _: (bool, Vec<(String, String, String)>) = self
            .manager()
            .await?
            .call("EnableUnitFiles", &(vec![unit], false, false))
            .await
            .map_err(|error| bus_failure(unit, error))?;
        done(vec![Action::DisableUnit {
            unit: unit.to_string(),
        }])
    }

    async fn disable(&self, unit: &str, prepared: &mut Prepared<'_>) -> Outcome {
        let facts = self.observe(unit).await?;
        if !facts.enabled {
            return Err(conflict(unit, "enabled", "disabled"));
        }
        prepared(&[Action::EnableUnit {
            unit: unit.to_string(),
        }])?;
        let _: Vec<(String, String, String)> = self
            .manager()
            .await?
            .call("DisableUnitFiles", &(vec![unit], false))
            .await
            .map_err(|error| bus_failure(unit, error))?;
        done(vec![Action::EnableUnit {
            unit: unit.to_string(),
        }])
    }

    async fn start(&self, unit: &str, scope: &Scope, prepared: &mut Prepared<'_>) -> Outcome {
        let facts = self.observe(unit).await?;
        if facts.active_state == "active" {
            return Err(conflict(unit, "inactive", "active"));
        }
        prepared(&[Action::StopUnit {
            unit: unit.to_string(),
        }])?;
        self.job("StartUnit", unit, scope).await?;
        done(vec![Action::StopUnit {
            unit: unit.to_string(),
        }])
    }

    async fn stop(&self, unit: &str, scope: &Scope, prepared: &mut Prepared<'_>) -> Outcome {
        let facts = self.observe(unit).await?;
        if facts.active_state != "active" {
            return Err(conflict(unit, "active", &facts.active_state));
        }
        prepared(&[Action::StartUnit {
            unit: unit.to_string(),
        }])?;
        self.job("StopUnit", unit, scope).await?;
        done(vec![Action::StartUnit {
            unit: unit.to_string(),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_unit_type_reads_its_result_from_its_own_interface() {
        assert_eq!(
            interface_of("nix-daemon.service"),
            Some("org.freedesktop.systemd1.Service")
        );
        assert_eq!(
            interface_of("nix-daemon.socket"),
            Some("org.freedesktop.systemd1.Socket")
        );
        assert_eq!(interface_of("multi-user.target"), None);
        assert_eq!(interface_of("no-suffix"), None);
    }

    #[test]
    fn a_missing_unit_is_a_unit_failure_and_a_refusal_is_permission_denied() {
        assert!(matches!(
            method_failure("nix-daemon.socket", "org.freedesktop.systemd1.NoSuchUnit", ""),
            Failure::Unit(unit) if unit.unit_result == "not-found"
        ));
        assert!(matches!(
            method_failure(
                "nix-daemon.socket",
                "org.freedesktop.DBus.Error.AccessDenied",
                ""
            ),
            Failure::Io {
                kind: std::io::ErrorKind::PermissionDenied,
                ..
            }
        ));
        assert_eq!(
            method_failure("x", "org.freedesktop.DBus.Error.ServiceUnknown", ""),
            Failure::SystemdUnreachable
        );
    }

    #[test]
    fn an_unknown_error_keeps_its_name_and_message() {
        assert_eq!(
            method_failure("x", "org.freedesktop.systemd1.JobTypeNotApplicable", "no"),
            Failure::CommandFailed {
                program: "systemd".into(),
                status: None,
                output_tail: "org.freedesktop.systemd1.JobTypeNotApplicable: no".into(),
            }
        );
    }

    #[test]
    fn an_invocation_id_is_written_as_journald_matches_it() {
        assert_eq!(invocation(&[0xe5, 0xbd, 0x0d]), Some("e5bd0d".to_string()));
        assert_eq!(invocation(&[]), None);
    }

    #[tokio::test]
    async fn a_unit_this_host_runs_is_observed_as_loaded_and_active() {
        let Ok(units) = Units::connect().await else {
            return;
        };
        let Ok(facts) = units.observe("dbus.socket").await else {
            return;
        };

        assert_eq!(facts.load_state, "loaded");
        assert_eq!(facts.active_state, "active");
    }
}
