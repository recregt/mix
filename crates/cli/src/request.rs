use mix_events::Fault;
use mix_events::mirror::Mirror;
use mix_events::v1::command::Request;
use mix_events::v1::{
    BootstrapRequest, CleanRequest, Code, DoctorRequest, ExplainRequest, InstallRequest,
    ListRequest, PurgeRequest, RemoveRequest, RepairRequest, SelfRemoveRequest, UpdateRequest,
};

use crate::args::{Command, Itself};
use crate::env::Environment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Socket,
    OneShot,
}

pub fn command_of(
    command: &Command,
    dry_run: bool,
    environment: &Environment,
) -> mix_events::v1::Command {
    mix_events::v1::Command {
        dry_run,
        ..mix_events::command(from_args(command, environment))
    }
}

pub fn from_args(command: &Command, environment: &Environment) -> Request {
    match command {
        Command::Bootstrap {
            mirror,
            mirror_key,
            force,
            ..
        } => {
            let (mirror, mirror_key) = match mirror {
                Some(url) => (Some(url.clone()), mirror_key.clone()),
                None => (
                    environment.mirror.clone(),
                    mirror_key
                        .clone()
                        .or_else(|| environment.mirror_key.clone()),
                ),
            };
            Request::Bootstrap(Box::new(BootstrapRequest {
                force: *force,
                mirror_key: mirror.as_ref().and(mirror_key),
                mirror,
            }))
        }
        Command::Install { packages, .. } => Request::Install(InstallRequest {
            packages: packages.clone(),
        }),
        Command::Remove { packages, .. } => Request::Remove(RemoveRequest {
            packages: packages.clone(),
        }),
        Command::Clean { all, .. } => Request::Clean(CleanRequest { all: *all }),
        Command::Repair { .. } => Request::Repair(RepairRequest {}),
        Command::Update { .. } => Request::Update(UpdateRequest {}),
        Command::Doctor => Request::Doctor(DoctorRequest {}),
        Command::Explain { code, .. } => Request::Explain(ExplainRequest {
            code: code.map(|code| code as i32),
        }),
        Command::List => Request::List(ListRequest {}),
        Command::Purge { .. } => Request::Purge(PurgeRequest {}),
        Command::Itself(Itself::Remove { .. }) => Request::SelfRemove(SelfRemoveRequest {}),
    }
}

pub fn route(request: &Request) -> Route {
    match request {
        Request::Bootstrap(_) | Request::Purge(_) => Route::OneShot,
        Request::Install(_)
        | Request::Remove(_)
        | Request::Clean(_)
        | Request::Repair(_)
        | Request::Update(_)
        | Request::Doctor(_)
        | Request::Explain(_)
        | Request::List(_)
        | Request::SelfRemove(_) => Route::Socket,
    }
}

pub fn refused(request: &Request) -> Option<Fault> {
    match request {
        Request::Bootstrap(bootstrap) => {
            Mirror::given(bootstrap.mirror.as_deref(), bootstrap.mirror_key.as_deref())
                .err()
                .map(|invalid| Fault::failed(Code::InvalidMirror, invalid.to_string(), None))
        }
        Request::Install(_)
        | Request::Remove(_)
        | Request::Clean(_)
        | Request::Repair(_)
        | Request::Update(_)
        | Request::Doctor(_)
        | Request::Explain(_)
        | Request::List(_)
        | Request::SelfRemove(_)
        | Request::Purge(_) => None,
    }
}

pub fn unconfirmed(
    command: &Command,
    interactive: bool,
    others: impl FnOnce() -> Vec<String>,
    ask: impl FnOnce(&str) -> bool,
) -> Option<Fault> {
    let (yes, dry_run, question) = match command {
        Command::Itself(Itself::Remove { yes, dry_run }) => (
            *yes,
            *dry_run,
            "This removes `mix` and Nix from your account. Continue?".to_string(),
        ),
        Command::Purge { yes, dry_run } => (*yes, *dry_run, purge_question(&others())),
        _ => return None,
    };
    if yes || dry_run {
        return None;
    }
    let message = match interactive {
        false => "there is no terminal to ask on and `--yes` was not given",
        true if ask(&question) => return None,
        true => "the answer was not yes",
    };
    Some(Fault::failed(Code::ConfirmationRequired, message, None))
}

pub fn leaves_the_machine(command: &Command, removal: Option<mix_render::Removal>) -> bool {
    matches!(
        command,
        Command::Itself(Itself::Remove { dry_run: false, .. })
    ) && removal.is_some_and(|removal| removal.others.is_empty() && !removal.machine)
}

fn purge_question(users: &[String]) -> String {
    match users {
        [] => "This removes `mix` and Nix from this machine. Continue?".to_string(),
        users => format!(
            "This removes `mix`, Nix and the `mix` state of {} from this machine. Continue?",
            users.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(url: Option<&str>, key: Option<&str>) -> Environment {
        Environment {
            mirror: url.map(str::to_string),
            mirror_key: key.map(str::to_string),
            ci: false,
        }
    }

    fn bootstrap(mirror: Option<&str>, mirror_key: Option<&str>) -> Command {
        Command::Bootstrap {
            dry_run: false,
            mirror: mirror.map(str::to_string),
            mirror_key: mirror_key.map(str::to_string),
            force: false,
        }
    }

    fn mirror_of(request: Request) -> (Option<String>, Option<String>) {
        match request {
            Request::Bootstrap(bootstrap) => (bootstrap.mirror, bootstrap.mirror_key),
            other => panic!("expected a bootstrap request, got {other:?}"),
        }
    }

    #[test]
    fn a_mirror_on_the_command_line_wins_over_the_environment_with_its_own_key() {
        let request = from_args(
            &bootstrap(Some("http://flag.internal"), None),
            &environment(Some("http://env.internal"), Some("env:KEY")),
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
            &environment(Some("http://env.internal"), Some("env:KEY")),
        );

        assert_eq!(
            mirror_of(request),
            (Some("http://env.internal".into()), Some("env:KEY".into()))
        );
        assert_eq!(
            mirror_of(from_args(&bootstrap(None, None), &environment(None, None))),
            (None, None)
        );
    }

    #[test]
    fn a_key_without_a_mirror_is_not_sent() {
        let request = from_args(
            &bootstrap(None, Some("stray:KEY")),
            &environment(None, None),
        );

        assert_eq!(mirror_of(request), (None, None));
    }

    #[test]
    fn only_bootstrap_and_purge_ask_for_sudo() {
        let bootstrap = from_args(&bootstrap(None, None), &Environment::default());
        assert_eq!(route(&bootstrap), Route::OneShot);
        let purge = from_args(
            &Command::Purge {
                yes: true,
                dry_run: false,
            },
            &Environment::default(),
        );
        assert_eq!(route(&purge), Route::OneShot);
        for command in [
            Command::Itself(Itself::Remove {
                yes: true,
                dry_run: false,
            }),
            Command::Doctor,
            Command::Repair { dry_run: false },
            Command::Update { dry_run: false },
            Command::Clean {
                all: true,
                dry_run: false,
            },
            Command::Explain {
                code: None,
                list: true,
            },
            Command::List,
        ] {
            assert_eq!(
                route(&from_args(&command, &Environment::default())),
                Route::Socket
            );
        }
    }

    #[test]
    fn a_bad_mirror_is_refused_before_sudo_is_asked() {
        let request = from_args(&bootstrap(Some("not a url"), None), &Environment::default());

        assert_eq!(
            refused(&request).and_then(|fault| fault.code()),
            Some(Code::InvalidMirror)
        );
    }

    #[test]
    fn every_command_builds_the_request_it_names() {
        let packages = vec!["ripgrep".to_string(), "fd".to_string()];
        for (command, request) in [
            (
                Command::Bootstrap {
                    dry_run: false,
                    mirror: None,
                    mirror_key: None,
                    force: true,
                },
                Request::Bootstrap(Box::new(BootstrapRequest {
                    force: true,
                    mirror: None,
                    mirror_key: None,
                })),
            ),
            (
                Command::Install {
                    dry_run: false,
                    packages: packages.clone(),
                },
                Request::Install(InstallRequest {
                    packages: packages.clone(),
                }),
            ),
            (
                Command::Remove {
                    dry_run: false,
                    packages: packages.clone(),
                },
                Request::Remove(RemoveRequest {
                    packages: packages.clone(),
                }),
            ),
            (
                Command::Clean {
                    all: true,
                    dry_run: false,
                },
                Request::Clean(CleanRequest { all: true }),
            ),
            (
                Command::Clean {
                    all: false,
                    dry_run: false,
                },
                Request::Clean(CleanRequest { all: false }),
            ),
            (
                Command::Repair { dry_run: false },
                Request::Repair(RepairRequest {}),
            ),
            (
                Command::Update { dry_run: false },
                Request::Update(UpdateRequest {}),
            ),
            (Command::Doctor, Request::Doctor(DoctorRequest {})),
            (
                Command::Explain {
                    code: Some(Code::Network),
                    list: false,
                },
                Request::Explain(ExplainRequest {
                    code: Some(Code::Network as i32),
                }),
            ),
            (
                Command::Explain {
                    code: None,
                    list: true,
                },
                Request::Explain(ExplainRequest { code: None }),
            ),
            (Command::List, Request::List(ListRequest {})),
            (
                Command::Itself(Itself::Remove {
                    yes: true,
                    dry_run: false,
                }),
                Request::SelfRemove(SelfRemoveRequest {}),
            ),
            (
                Command::Purge {
                    yes: true,
                    dry_run: false,
                },
                Request::Purge(PurgeRequest {}),
            ),
        ] {
            assert_eq!(from_args(&command, &Environment::default()), request);
        }
    }

    #[test]
    fn a_removal_asks_on_a_terminal_and_refuses_without_one() {
        let remove = |yes, dry_run| Command::Itself(Itself::Remove { yes, dry_run });
        let purge = |yes, dry_run| Command::Purge { yes, dry_run };
        let refused = |fault: Option<Fault>| fault.and_then(|fault| fault.code());
        let nobody = Vec::new;

        for command in [remove, purge] {
            assert_eq!(
                refused(unconfirmed(&command(true, false), false, nobody, |_| false)),
                None
            );
            assert_eq!(
                refused(unconfirmed(&command(false, true), false, nobody, |_| false)),
                None
            );
            assert_eq!(
                refused(unconfirmed(&command(false, false), true, nobody, |_| true)),
                None
            );
            assert_eq!(
                refused(unconfirmed(&command(false, false), true, nobody, |_| false)),
                Some(Code::ConfirmationRequired)
            );
            assert_eq!(
                refused(unconfirmed(&command(false, false), false, nobody, |_| {
                    panic!("asked")
                })),
                Some(Code::ConfirmationRequired)
            );
        }
        assert_eq!(
            refused(unconfirmed(&Command::List, false, nobody, |_| false)),
            None
        );
    }

    #[test]
    fn only_the_last_user_to_leave_is_offered_the_machine() {
        let remove = |dry_run| Command::Itself(Itself::Remove { yes: true, dry_run });
        let alone = Some(mix_render::Removal {
            others: Vec::new(),
            machine: false,
        });
        let shared = Some(mix_render::Removal {
            others: vec!["bob".to_string()],
            machine: false,
        });
        let purged = Some(mix_render::Removal {
            others: Vec::new(),
            machine: true,
        });

        assert!(leaves_the_machine(&remove(false), alone.clone()));
        assert!(!leaves_the_machine(&remove(false), shared));
        assert!(!leaves_the_machine(&remove(false), purged));
        assert!(!leaves_the_machine(&remove(false), None));
        assert!(!leaves_the_machine(&remove(true), alone));
        assert!(!leaves_the_machine(&Command::List, None));
    }

    #[test]
    fn a_purge_names_every_user_it_removes_in_its_question() {
        let asked = std::cell::RefCell::new(String::new());
        let purge = Command::Purge {
            yes: false,
            dry_run: false,
        };

        let users = || vec!["alice".to_string(), "bob".to_string()];
        unconfirmed(&purge, true, users, |question| {
            *asked.borrow_mut() = question.to_string();
            true
        });

        assert!(asked.borrow().contains("alice, bob"), "{}", asked.borrow());
        let alone = std::cell::RefCell::new(String::new());
        unconfirmed(&purge, true, Vec::new, |question| {
            *alone.borrow_mut() = question.to_string();
            true
        });
        assert!(!alone.borrow().contains("state of"), "{}", alone.borrow());
    }

    #[test]
    fn a_removal_never_names_other_users() {
        let remove = Command::Itself(Itself::Remove {
            yes: false,
            dry_run: false,
        });

        unconfirmed(
            &remove,
            true,
            || panic!("a user removal must not look up other users"),
            |question| {
                assert!(question.contains("your account"));
                true
            },
        );
    }
}
