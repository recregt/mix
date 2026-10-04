use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::action::{Action, Fact, Failure, Outcome, Performed, ProfileFacts, Query};
use mix_core::identity::InvokingUser;
use mix_core::paths::{
    DEFAULT_PROFILE_NIX_ENV, DEFAULT_PROFILE_NIX_STORE, HOME_MANAGER_PROFILE_NAME, nix_profiles_dir,
};
use mix_exec::Scope;

use crate::effect::exec::{run_as, run_as_reporting};
use crate::effect::files::Prepared;
use crate::effect::tools::trusted;
use crate::profile;

pub struct ProfileContext {
    pub mirror: Option<String>,
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
    match query {
        Query::Profile(user) => {
            let mut generations = existing(user);
            generations.sort_unstable();
            let dangling = generations
                .iter()
                .copied()
                .filter(|generation| std::fs::metadata(generation_link(user, *generation)).is_err())
                .collect();
            Some(Fact::Profile(ProfileFacts {
                generations,
                active: current(user),
                dangling,
            }))
        }
        Query::Clobbered(user) => Some(Fact::Clobbered(clobbered(user))),
        _ => None,
    }
}

fn generation_link(user: &InvokingUser, generation: u64) -> PathBuf {
    nix_profiles_dir(&user.home).join(format!("{HOME_MANAGER_PROFILE_NAME}-{generation}-link"))
}

/// Files of the active generation's `home-files` tree: what home-manager links into the home.
const HOME_FILES: &str = "home-files";

/// Paths in the user's home where the active generation links a file and something else is:
/// a file, a directory, or a link into anything but that generation's files.
fn clobbered(user: &InvokingUser) -> Vec<PathBuf> {
    let Ok(generation) = std::fs::canonicalize(profile_link(user)) else {
        return Vec::new();
    };
    let managed = generation.join(HOME_FILES);
    let Ok(files) = std::fs::canonicalize(&managed) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    in_the_way(&managed, &user.home, &[&files, &managed], &mut found);
    found.sort();
    found
}

fn in_the_way(managed: &Path, home: &Path, ours: &[&Path], found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(managed) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let source = entry.path();
        let destination = home.join(entry.file_name());
        let Ok(there) = std::fs::symlink_metadata(&destination) else {
            continue;
        };
        if std::fs::metadata(&source).is_ok_and(|source| source.is_dir()) {
            if there.is_dir() {
                in_the_way(&source, &destination, ours, found);
            } else {
                found.push(destination);
            }
            continue;
        }
        let linked = there.file_type().is_symlink()
            && std::fs::read_link(&destination)
                .is_ok_and(|target| ours.iter().any(|files| target.starts_with(files)));
        if !linked {
            found.push(destination);
        }
    }
}

fn current(user: &InvokingUser) -> Option<u64> {
    let target = std::fs::read_link(profile_link(user)).ok()?;
    generation_of(target.file_name()?.to_str()?)
}

pub(crate) fn existing(user: &InvokingUser) -> Vec<u64> {
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
    built: Option<u64>,
    context: &ProfileContext,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Option<Outcome> {
    if let Action::ActivateProfile { user, .. }
    | Action::SwitchGeneration { user, .. }
    | Action::DeleteGeneration { user, .. }
    | Action::ApplyGeneration { user } = action
        && let Err(failure) = crate::effect::profile_lock::wait(user, activity, scope).await
    {
        return Some(Err(failure));
    }
    Some(match action {
        Action::ActivateProfile { user, source } => {
            activate(user, *source, built, context, activity, scope, prepared).await
        }
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
                Ok(()) => apply(user, activity, scope)
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
        Action::CollectGarbage { user } => match prepared(&[]) {
            Err(failure) => Err(failure),
            Ok(()) => run_as_reporting(
                user,
                DEFAULT_PROFILE_NIX_STORE,
                &["--gc"],
                scope,
                Some(Arc::clone(activity)),
            )
            .await
            .map(|_| Performed { undo: Vec::new() })
            .map_err(core_failure),
        },
        _ => return None,
    })
}

async fn apply(
    user: &InvokingUser,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
) -> Result<(), Failure> {
    let Ok(target) = std::fs::canonicalize(profile_link(user)) else {
        return Ok(());
    };
    profile::activate_generation(user, &target.to_string_lossy(), activity, scope)
        .await
        .map_err(core_failure)
}

async fn reuse(
    user: &InvokingUser,
    previous: Option<u64>,
    built: u64,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Outcome {
    let moved = previous != Some(built);
    let undo = if moved {
        vec![
            Action::SwitchGeneration {
                user: user.clone(),
                generation: previous,
                expect: Some(built),
            },
            Action::ApplyGeneration { user: user.clone() },
        ]
    } else {
        Vec::new()
    };
    prepared(&undo)?;
    if moved {
        switch_to(user, Some(built), scope).await?;
    }
    if let Err(failure) = apply(user, activity, &scope.shielded()).await {
        if moved {
            let shielded = scope.shielded();
            let _ = switch_to(user, previous, &shielded).await;
            let _ = apply(user, activity, &shielded).await;
        }
        return Err(failure);
    }
    Ok(Performed { undo })
}

async fn activate(
    user: &InvokingUser,
    source: mix_core::action::FlakeSource,
    built: Option<u64>,
    context: &ProfileContext,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Outcome {
    let previous = current(user);
    let before = existing(user);
    if let Some(built) = built.filter(|built| before.contains(built)) {
        return reuse(user, previous, built, activity, scope, prepared).await;
    }
    let predicted = before.iter().max().map_or(1, |last| last + 1);
    prepared(&undo_activation(user, previous, predicted))?;
    let generation = profile::switch(user, source, context.mirror.as_deref(), activity, scope)
        .await
        .map_err(core_failure)?;
    let new = current(user).unwrap_or(predicted);
    if let Err(error) =
        profile::activate_generation(user, &generation, activity, &scope.shielded()).await
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
            let _ = apply(user, activity, &shielded).await;
        }
        return Err(core_failure(error));
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
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn user_in(home: &Path) -> InvokingUser {
        InvokingUser {
            uid: nix::unistd::Uid::current().as_raw(),
            gid: nix::unistd::Gid::current().as_raw(),
            name: "mix-user".into(),
            home: home.to_path_buf(),
        }
    }

    #[test]
    fn a_generation_whose_link_no_longer_resolves_is_dangling() {
        let home = tempfile::tempdir().unwrap();
        let user = user_in(home.path());
        let generation = home.path().join("generation-2");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::create_dir_all(nix_profiles_dir(&user.home)).unwrap();
        std::os::unix::fs::symlink(home.path().join("gone"), generation_link(&user, 1)).unwrap();
        std::os::unix::fs::symlink(&generation, generation_link(&user, 2)).unwrap();
        std::os::unix::fs::symlink("home-manager-2-link", profile_link(&user)).unwrap();

        let Some(Fact::Profile(profile)) = observe(&Query::Profile(user)) else {
            panic!("a profile is observed");
        };

        assert_eq!(profile.generations, [1, 2]);
        assert_eq!(profile.active, Some(2));
        assert_eq!(profile.dangling, [1]);
    }

    #[test]
    fn only_what_is_not_a_link_into_the_generation_where_a_managed_file_goes_is_in_the_way() {
        let home = tempfile::tempdir().unwrap();
        let user = user_in(home.path());
        let generation = home.path().join("store/generation");
        let managed = generation.join(HOME_FILES);
        std::fs::create_dir_all(managed.join(".config/app")).unwrap();
        for file in [
            ".bashrc",
            ".profile",
            ".inputrc",
            ".gitconfig",
            ".config/app/settings",
        ] {
            std::fs::write(managed.join(file), "managed").unwrap();
        }
        std::fs::create_dir_all(nix_profiles_dir(&user.home)).unwrap();
        std::os::unix::fs::symlink(&generation, profile_link(&user)).unwrap();
        std::fs::write(home.path().join(".bashrc"), "mine").unwrap();
        std::os::unix::fs::symlink(managed.join(".profile"), home.path().join(".profile")).unwrap();
        std::os::unix::fs::symlink("/nix/store/y-other/inputrc", home.path().join(".inputrc"))
            .unwrap();
        std::fs::create_dir_all(home.path().join(".config/app/settings")).unwrap();

        let Some(Fact::Clobbered(found)) = observe(&Query::Clobbered(user)) else {
            panic!("the home is observed");
        };

        assert_eq!(
            found,
            [
                home.path().join(".bashrc"),
                home.path().join(".config/app/settings"),
                home.path().join(".inputrc")
            ]
        );
    }

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
