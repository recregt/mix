use std::path::PathBuf;
use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::action::{Action, Fact, Failure, Outcome, Performed, ProfileFacts, Query};
use mix_core::paths::{DEFAULT_PROFILE_NIX_ENV, HOME_MANAGER_PROFILE_NAME, nix_profiles_dir};
use mix_core::privilege::InvokingUser;
use mix_exec::Scope;

use crate::HostConfig;
use crate::effect::exec::{run_as, run_as_reporting};
use crate::effect::files::Prepared;
use crate::effect::tools::trusted;
use crate::profile::{self, BuildPolicy};

pub struct ProfileContext {
    pub mirror: Option<String>,
    pub activity: Arc<dyn ActivityReporter>,
    pub host: HostConfig,
}

pub fn profile_link(user: &InvokingUser) -> PathBuf {
    nix_profiles_dir(&user.home).join(HOME_MANAGER_PROFILE_NAME)
}

pub fn generation_of(link_name: &str) -> Option<u64> {
    link_name
        .strip_prefix(HOME_MANAGER_PROFILE_NAME)?
        .strip_prefix('-')?
        .strip_suffix("-link")?
        .parse()
        .ok()
}

pub fn observe(query: &Query) -> Option<Fact> {
    let Query::Profile(user) = query else {
        return None;
    };
    let mut generations = existing(user);
    generations.sort_unstable();
    Some(Fact::Profile(ProfileFacts {
        generations,
        active: current(user),
    }))
}

fn current(user: &InvokingUser) -> Option<u64> {
    let target = std::fs::read_link(profile_link(user)).ok()?;
    generation_of(target.file_name()?.to_str()?)
}

fn existing(user: &InvokingUser) -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(nix_profiles_dir(&user.home)) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| generation_of(entry.file_name().to_str()?))
        .collect()
}

pub fn core_failure(error: mix_core::Error) -> Failure {
    match error {
        mix_core::Error::Cancelled { .. } => Failure::Cancelled,
        mix_core::Error::Io { path, source } => Failure::Io {
            path,
            kind: source.kind(),
        },
        mix_core::Error::Exec { command, source } => Failure::SpawnFailed {
            program: command,
            kind: source.kind(),
        },
        mix_core::Error::Command { command, detail } => Failure::CommandFailed {
            program: command,
            status: None,
            output_tail: detail,
        },
        other => Failure::CommandFailed {
            program: "mix".to_string(),
            status: None,
            output_tail: other.to_string(),
        },
    }
}

fn profile_failure(error: profile::Error) -> Failure {
    match error {
        profile::Error::Core(error) => core_failure(error),
        other => Failure::CommandFailed {
            program: "nix build".to_string(),
            status: None,
            output_tail: other.to_string(),
        },
    }
}

fn undo_activation(user: &InvokingUser, previous: Option<u64>, new: u64) -> Vec<Action> {
    vec![
        Action::SwitchGeneration {
            user: user.clone(),
            generation: previous,
            expect: Some(new),
        },
        Action::DeleteGeneration {
            user: user.clone(),
            generation: new,
        },
        Action::ApplyGeneration { user: user.clone() },
    ]
}

async fn switch_to(
    user: &InvokingUser,
    generation: Option<u64>,
    scope: &Scope,
) -> Result<(), Failure> {
    let link = profile_link(user);
    let link = link.to_string_lossy();
    match generation {
        Some(generation) => {
            let generation = generation.to_string();
            run_as(
                user,
                DEFAULT_PROFILE_NIX_ENV,
                &["--profile", &link, "--switch-generation", &generation],
                scope,
            )
            .await
            .map(|_| ())
            .map_err(core_failure)
        }
        None => {
            let rm = trusted("rm")?;
            run_as(user, &rm.to_string_lossy(), &["-f", &link], scope)
                .await
                .map(|_| ())
                .map_err(core_failure)
        }
    }
}

pub async fn perform(
    action: &Action,
    context: &ProfileContext,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Option<Outcome> {
    Some(match action {
        Action::ActivateProfile {
            user,
            allow_source_builds,
        } => activate(user, *allow_source_builds, context, scope, prepared).await,
        Action::SwitchGeneration {
            user,
            generation,
            expect,
        } => {
            let found = current(user);
            if found == *generation {
                return Some(Ok(Performed { undo: Vec::new() }));
            }
            if found != *expect {
                return Some(Err(Failure::Conflict {
                    subject: profile_link(user).display().to_string(),
                    expected: format!("generation {expect:?}"),
                    found: format!("generation {found:?}"),
                }));
            }
            let undo = vec![Action::SwitchGeneration {
                user: user.clone(),
                generation: *expect,
                expect: *generation,
            }];
            match prepared(&undo) {
                Err(failure) => Err(failure),
                Ok(()) => switch_to(user, *generation, scope)
                    .await
                    .map(|()| Performed { undo }),
            }
        }
        Action::ApplyGeneration { user } => {
            let undo = vec![Action::ApplyGeneration { user: user.clone() }];
            match prepared(&undo) {
                Err(failure) => Err(failure),
                Ok(()) => apply(user, context, scope)
                    .await
                    .map(|()| Performed { undo }),
            }
        }
        Action::DeleteGeneration { user, generation } => {
            let link = profile_link(user);
            let generation = generation.to_string();
            match prepared(&[]) {
                Err(failure) => Err(failure),
                Ok(()) => run_as(
                    user,
                    DEFAULT_PROFILE_NIX_ENV,
                    &[
                        "--profile",
                        &link.to_string_lossy(),
                        "--delete-generations",
                        &generation,
                    ],
                    scope,
                )
                .await
                .map(|_| Performed { undo: Vec::new() })
                .map_err(core_failure),
            }
        }
        _ => return None,
    })
}

async fn apply(
    user: &InvokingUser,
    context: &ProfileContext,
    scope: &Scope,
) -> Result<(), Failure> {
    let Ok(target) = std::fs::canonicalize(profile_link(user)) else {
        return Ok(());
    };
    profile::activate_generation(user, &target.to_string_lossy(), &context.activity, scope)
        .await
        .map_err(profile_failure)
}

async fn activate(
    user: &InvokingUser,
    allow_source_builds: bool,
    context: &ProfileContext,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Outcome {
    let previous = current(user);
    let before = existing(user);
    let predicted = before.iter().max().map_or(1, |last| last + 1);
    prepared(&undo_activation(user, previous, predicted))?;
    let policy = if allow_source_builds {
        BuildPolicy::AllowSource
    } else {
        BuildPolicy::CacheOnly
    };
    let generation = profile::switch(
        user,
        context.mirror.as_deref(),
        &context.activity,
        scope,
        policy,
    )
    .await
    .map_err(profile_failure)?;
    let new = current(user).unwrap_or(predicted);
    if let Err(error) =
        profile::activate_generation(user, &generation, &context.activity, scope).await
    {
        let shielded = scope.shielded();
        if previous != Some(new) {
            let _ = switch_to(user, previous, &shielded).await;
        }
        if !before.contains(&new) {
            let _ = run_as_reporting(
                user,
                DEFAULT_PROFILE_NIX_ENV,
                &[
                    "--profile",
                    &profile_link(user).to_string_lossy(),
                    "--delete-generations",
                    &new.to_string(),
                ],
                &shielded,
                None,
            )
            .await;
        }
        if previous != Some(new) {
            let _ = apply(user, context, &shielded).await;
        }
        return Err(profile_failure(error));
    }
    Ok(Performed {
        undo: if previous == Some(new) {
            Vec::new()
        } else if before.contains(&new) {
            vec![
                Action::SwitchGeneration {
                    user: user.clone(),
                    generation: previous,
                    expect: Some(new),
                },
                Action::ApplyGeneration { user: user.clone() },
            ]
        } else {
            undo_activation(user, previous, new)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generation_number_is_read_from_its_link_name() {
        assert_eq!(generation_of("home-manager-1-link"), Some(1));
        assert_eq!(generation_of("home-manager-42-link"), Some(42));
        assert_eq!(generation_of("home-manager"), None);
        assert_eq!(generation_of("home-manager-x-link"), None);
        assert_eq!(generation_of("other-3-link"), None);
    }

    #[test]
    fn undoing_an_activation_switches_back_deletes_the_new_generation_then_applies() {
        let user = InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".into(),
            home: "/home/alice".into(),
        };

        assert_eq!(
            undo_activation(&user, None, 1),
            [
                Action::SwitchGeneration {
                    user: user.clone(),
                    generation: None,
                    expect: Some(1)
                },
                Action::DeleteGeneration {
                    user: user.clone(),
                    generation: 1
                },
                Action::ApplyGeneration { user }
            ]
        );
    }
}
