use std::fmt::{self, Display};

use mix_events::Fault;
use mix_events::v1::command::Request;
use mix_events::v1::{
    BootstrapRequest, CleanRequest, Code, DoctorRequest, InstallRequest, RemoveRequest,
    RepairRequest,
};
use mix_ui::{Note, note};

use crate::args::Command;

pub const MIRROR_VAR: &str = "MIX_NIX_MIRROR";
pub const MIRROR_KEY_VAR: &str = "MIX_NIX_MIRROR_KEY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Socket,
    OneShot,
}

pub fn from_args(
    command: &Command,
    environment: impl Fn(&str) -> Option<String>,
) -> Option<Request> {
    Some(match command {
        Command::Bootstrap {
            mirror,
            mirror_key,
            force,
        } => {
            let (mirror, mirror_key) = match mirror {
                Some(url) => (Some(url.clone()), mirror_key.clone()),
                None => (
                    environment(MIRROR_VAR),
                    mirror_key.clone().or_else(|| environment(MIRROR_KEY_VAR)),
                ),
            };
            Request::Bootstrap(Box::new(BootstrapRequest {
                force: *force,
                mirror_key: mirror.as_ref().and(mirror_key),
                mirror,
            }))
        }
        Command::Install { packages } => Request::Install(InstallRequest {
            packages: packages.clone(),
        }),
        Command::Remove { packages } => Request::Remove(RemoveRequest {
            packages: packages.clone(),
        }),
        Command::Clean { all } => Request::Clean(CleanRequest { all: *all }),
        Command::Repair => Request::Repair(RepairRequest {}),
        Command::Doctor => Request::Doctor(DoctorRequest {}),
        Command::Explain { .. } | Command::Events { .. } => return None,
    })
}

pub fn name(request: &Request) -> &'static str {
    match request {
        Request::Bootstrap(_) => "mix bootstrap",
        Request::Install(_) => "mix install",
        Request::Remove(_) => "mix remove",
        Request::Clean(_) => "mix clean",
        Request::Repair(_) => "mix repair",
        Request::Doctor(_) => "mix doctor",
    }
}

pub fn action(request: &Request) -> Box<dyn Display + '_> {
    match request {
        Request::Bootstrap(_) => Box::new("finish setting up `mix`"),
        Request::Install(install) => Box::new(Packages("install", &install.packages)),
        Request::Remove(remove) => Box::new(Packages("remove", &remove.packages)),
        Request::Clean(_) => Box::new("clean up your profile"),
        Request::Repair(_) => Box::new("finish the repair"),
        Request::Doctor(_) => Box::new("finish the health check"),
    }
}

pub fn route(request: &Request) -> Route {
    match request {
        Request::Bootstrap(_) => Route::OneShot,
        Request::Install(_)
        | Request::Remove(_)
        | Request::Clean(_)
        | Request::Repair(_)
        | Request::Doctor(_) => Route::Socket,
    }
}

pub fn stopping(request: &Request) -> Option<Note> {
    match request {
        Request::Bootstrap(_) => Some(note!("cancelling and cleaning up")),
        Request::Install(_) | Request::Remove(_) => {
            Some(note!("cancelling and putting the package list back"))
        }
        Request::Clean(_) => Some(note!("stopping after the current removal")),
        Request::Repair(_) => Some(note!("stopping after the current repair")),
        Request::Doctor(_) => None,
    }
}

pub fn refused(request: &Request) -> Option<Fault> {
    match request {
        Request::Bootstrap(bootstrap) => mix_core::policy::Policy::new(
            bootstrap.mirror.as_deref(),
            bootstrap.mirror_key.as_deref(),
        )
        .err()
        .map(|invalid| mix_core::diagnose::failed(Code::InvalidMirror, invalid.to_string(), None)),
        Request::Install(_)
        | Request::Remove(_)
        | Request::Clean(_)
        | Request::Repair(_)
        | Request::Doctor(_) => None,
    }
}

struct Packages<'a>(&'static str, &'a [String]);

impl Display for Packages<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Packages(verb, packages) = self;
        f.write_str(verb)?;
        match packages {
            [] => f.write_str(" the packages"),
            [first, rest @ ..] if rest.len() < 3 => {
                f.write_str(" ")?;
                f.write_str(first)?;
                rest.iter().try_for_each(|package| {
                    f.write_str(", ")?;
                    f.write_str(package)
                })
            }
            packages => write!(f, " {} packages", packages.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(
        url: Option<&'static str>,
        key: Option<&'static str>,
    ) -> impl Fn(&str) -> Option<String> {
        move |name| match name {
            MIRROR_VAR => url.map(str::to_string),
            MIRROR_KEY_VAR => key.map(str::to_string),
            _ => None,
        }
    }

    fn bootstrap(mirror: Option<&str>, mirror_key: Option<&str>) -> Command {
        Command::Bootstrap {
            mirror: mirror.map(str::to_string),
            mirror_key: mirror_key.map(str::to_string),
            force: false,
        }
    }

    fn mirror_of(request: Option<Request>) -> (Option<String>, Option<String>) {
        match request {
            Some(Request::Bootstrap(bootstrap)) => (bootstrap.mirror, bootstrap.mirror_key),
            other => panic!("expected a bootstrap request, got {other:?}"),
        }
    }

    #[test]
    fn a_mirror_on_the_command_line_wins_over_the_environment_with_its_own_key() {
        let request = from_args(
            &bootstrap(Some("http://flag.internal"), None),
            environment(Some("http://env.internal"), Some("env:KEY")),
        );

        assert_eq!(
            mirror_of(request),
            (Some("http://flag.internal".into()), None)
        );
    }

    #[test]
    fn a_mirror_set_only_in_the_environment_is_used_with_the_environments_key() {
        let request = from_args(
            &bootstrap(None, None),
            environment(Some("http://env.internal"), Some("env:KEY")),
        );

        assert_eq!(
            mirror_of(request),
            (Some("http://env.internal".into()), Some("env:KEY".into()))
        );
        assert_eq!(
            mirror_of(from_args(&bootstrap(None, None), environment(None, None))),
            (None, None)
        );
    }

    #[test]
    fn a_key_without_a_mirror_is_not_sent() {
        let request = from_args(&bootstrap(None, Some("stray:KEY")), environment(None, None));

        assert_eq!(mirror_of(request), (None, None));
    }

    #[test]
    fn only_explain_and_events_stay_on_this_machine() {
        assert!(
            from_args(
                &Command::Explain {
                    code: None,
                    list: true
                },
                |_| None
            )
            .is_none()
        );
        assert!(from_args(&Command::Doctor, |_| None).is_some());
    }

    #[test]
    fn only_bootstrap_asks_for_sudo() {
        let bootstrap = from_args(&bootstrap(None, None), |_| None).unwrap();
        assert_eq!(route(&bootstrap), Route::OneShot);
        for command in [
            Command::Doctor,
            Command::Repair,
            Command::Clean { all: true },
        ] {
            assert_eq!(
                route(&from_args(&command, |_| None).unwrap()),
                Route::Socket
            );
        }
    }

    #[test]
    fn a_bad_mirror_is_refused_before_sudo_is_asked() {
        let request = from_args(&bootstrap(Some("not a url"), None), |_| None).unwrap();

        assert_eq!(
            refused(&request).and_then(|fault| fault.code()),
            Some(Code::InvalidMirror)
        );
    }

    #[test]
    fn a_long_list_of_packages_is_counted() {
        let packages: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();

        assert_eq!(Packages("install", &packages[..1]).to_string(), "install a");
        assert_eq!(
            Packages("install", &packages[..3]).to_string(),
            "install a, b, c"
        );
        assert_eq!(
            Packages("install", &packages).to_string(),
            "install 4 packages"
        );
    }
}
