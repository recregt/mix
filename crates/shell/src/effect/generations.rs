use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::{DEFAULT_PROFILE_NIX_ENV, DEFAULT_PROFILE_NIX_STORE};
use mix_core::effect::{Action, Fact, Failure, Outcome, Performed, ProfileFacts, Query};
use mix_exec::Scope;
use mix_fs::Prepared;
use mix_home::{Active, Home, profile_link};
use mix_nixgen::Pins;
use mix_nixgen::lock::LockedInput;

use crate::effect::exec::{run_as, run_as_reporting};
use crate::effect::tools::trusted;
use crate::profile;

#[derive(Debug, Clone)]
pub struct ProfileContext {
    pub mirror: Option<String>,
}

pub fn pins(active: &Active) -> Option<Pins<LockedInput>> {
    mix_core::declared::pins::read(active.lock.as_deref()?)
}

pub fn wire(active: &Active) -> mix_events::v1::ActiveGeneration {
    mix_events::v1::ActiveGeneration {
        number: active.number,
        store_path: active.path.display().to_string(),
    }
}

async fn profile(home: &mut Home<'_>, user: &InvokingUser) -> Result<ProfileFacts, Failure> {
    match home.observe(&Query::Profile(user.clone())).await? {
        Some(Fact::Profile(found)) => Ok(found),
        _ => Ok(ProfileFacts::default()),
    }
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

fn out_of_inodes(failure: Failure, no_inodes: impl FnOnce() -> bool) -> Failure {
    match failure {
        Failure::CommandFailed {
            program,
            status,
            output_tail,
        } if mix_nixlog::nix_error(&output_tail).and_then(|error| error.failure)
            == Some(mix_nixlog::NixFailure::DiskFull)
            && no_inodes() =>
        {
            Failure::NoInodes {
                program,
                status,
                output_tail,
            }
        }
        other => other,
    }
}

fn exhausted(files: u64, free: u64) -> bool {
    files > 0 && free == 0
}

fn store_has_no_inodes() -> bool {
    rustix::fs::statvfs(mix_core::declared::paths::NIX_STORE)
        .is_ok_and(|stat| exhausted(stat.f_files, stat.f_ffree))
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

pub fn user_of(action: &Action) -> Option<&InvokingUser> {
    match action {
        Action::ActivateProfile { user, .. }
        | Action::SwitchGeneration { user, .. }
        | Action::DeleteGeneration { user, .. }
        | Action::ApplyGeneration { user }
        | Action::CollectGarbage { user } => Some(user),
        _ => None,
    }
}

pub async fn perform(
    action: &Action,
    built: Option<u64>,
    context: &ProfileContext,
    activity: &Arc<dyn ActivityReporter>,
    home: &mut Home<'_>,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Option<Outcome> {
    if let Action::ActivateProfile { user, .. }
    | Action::SwitchGeneration { user, .. }
    | Action::DeleteGeneration { user, .. }
    | Action::ApplyGeneration { user } = action
    {
        let mut on_wait = |waiting: mix_home::Waiting| {
            activity.waiting(
                &waiting.lock.display().to_string(),
                waiting.holder.as_deref(),
                waiting.command.as_deref(),
            );
        };
        if let Err(failure) = home.wait_profile_locks(user, &mut on_wait, scope).await {
            return Some(Err(failure));
        }
    }
    Some(match action {
        Action::ActivateProfile { user, source } => {
            activate(
                user, *source, built, context, activity, home, scope, prepared,
            )
            .await
        }
        Action::SwitchGeneration {
            user,
            generation,
            expect,
        } => {
            let found = match profile(home, user).await {
                Ok(found) => found.active,
                Err(failure) => return Some(Err(failure)),
            };
            if found == *generation {
                return Some(Ok(Performed { undo: Vec::new() }));
            }
            if found != *expect {
                return Some(Err(Failure::Conflict {
                    subject: profile_link(user).display().to_string(),
                    expected: mix_core::effect::generation(*expect),
                    found: mix_core::effect::generation(found),
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
                Ok(()) => apply(user, home, activity, scope)
                    .await
                    .map(|()| Performed { undo }),
            }
        }
        Action::DeleteGeneration { user, generation } => {
            let active = match profile(home, user).await {
                Ok(found) => found.active,
                Err(failure) => return Some(Err(failure)),
            };
            if active == Some(*generation) {
                return Some(Ok(Performed { undo: Vec::new() }));
            }
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
    home: &mut Home<'_>,
    activity: &Arc<dyn ActivityReporter>,
    scope: &Scope,
) -> Result<(), Failure> {
    let Some(target) = home.generation(user).await? else {
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
    home: &mut Home<'_>,
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
    if let Err(failure) = apply(user, home, activity, &scope.shielded()).await {
        if moved {
            let shielded = scope.shielded();
            let _ = switch_to(user, previous, &shielded).await;
            let _ = apply(user, home, activity, &shielded).await;
        }
        return Err(failure);
    }
    Ok(Performed { undo })
}

#[allow(clippy::too_many_arguments)]
async fn activate(
    user: &InvokingUser,
    source: mix_core::effect::FlakeSource,
    built: Option<u64>,
    context: &ProfileContext,
    activity: &Arc<dyn ActivityReporter>,
    home: &mut Home<'_>,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Outcome {
    let found = profile(home, user).await?;
    let previous = found.active;
    let before = found.generations;
    if let Some(built) = built.filter(|built| before.contains(built)) {
        return reuse(user, previous, built, activity, home, scope, prepared).await;
    }
    let predicted = before.iter().max().map_or(1, |last| last + 1);
    prepared(&undo_activation(user, previous, predicted))?;
    let generation = profile::switch(user, source, context.mirror.as_deref(), activity, scope)
        .await
        .map_err(|error| out_of_inodes(core_failure(error), store_has_no_inodes))?;
    let new = profile(home, user).await?.active.unwrap_or(predicted);
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
            let _ = apply(user, home, activity, &shielded).await;
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
    use std::path::{Path, PathBuf};

    use mix_core::declared::paths::nix_profiles_dir;

    use super::*;

    const OUT_OF_INODES: &str = "error: creating file \"/nix/store/8sjgd7q3mdks1rb7rbcv0sallhvrhai5-gcc-15.3.0/lib/liblsan.a\": No space left on device";

    fn build_failed(output_tail: &str) -> Failure {
        Failure::CommandFailed {
            program: "nix build".into(),
            status: Some(1),
            output_tail: output_tail.into(),
        }
    }

    #[test]
    fn a_full_disk_with_no_inodes_left_is_reported_as_out_of_inodes() {
        assert_eq!(
            out_of_inodes(build_failed(OUT_OF_INODES), || true),
            Failure::NoInodes {
                program: "nix build".into(),
                status: Some(1),
                output_tail: OUT_OF_INODES.into(),
            }
        );
    }

    #[test]
    fn a_full_disk_with_inodes_left_stays_a_full_disk() {
        assert_eq!(
            out_of_inodes(build_failed(OUT_OF_INODES), || false),
            build_failed(OUT_OF_INODES)
        );
    }

    #[test]
    fn a_failure_that_is_not_a_full_disk_never_looks_at_the_disk() {
        let unknown = "error: attribute 'ripgrep2' missing";

        assert_eq!(
            out_of_inodes(build_failed(unknown), || unreachable!(
                "only a full disk is checked"
            )),
            build_failed(unknown)
        );
    }

    #[test]
    fn inodes_run_out_only_on_a_file_system_that_counts_them() {
        assert!(exhausted(6_000, 0));
        assert!(!exhausted(6_000, 1));
        assert!(!exhausted(0, 0));
    }

    #[test]
    fn the_pins_of_a_generation_are_read_from_its_own_lock() {
        let rendered = mix_nixgen::lock::render(mix_nixgen::lock::EMBEDDED);
        let active = Active {
            number: 1,
            path: PathBuf::from("/nix/store/aaa-home-manager-generation"),
            list: None,
            lock: Some(rendered),
        };
        let broken = Active {
            lock: Some("{".to_string()),
            ..active.clone()
        };

        let unlocked = Active {
            lock: None,
            ..active.clone()
        };

        assert_eq!(pins(&active), Some(mix_nixgen::lock::EMBEDDED));
        assert_eq!(pins(&broken), None);
        assert_eq!(pins(&unlocked), None);
    }

    fn user_in(home: &Path) -> InvokingUser {
        InvokingUser {
            uid: nix::unistd::Uid::current().as_raw(),
            gid: nix::unistd::Gid::current().as_raw(),
            name: "mix-user".into(),
            home: home.to_path_buf(),
        }
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

    #[tokio::test]
    async fn switching_back_from_a_generation_someone_moved_is_a_conflict() {
        let home = tempfile::tempdir().unwrap();
        let user = user_in(home.path());
        let profiles = nix_profiles_dir(&user.home);
        std::fs::create_dir_all(&profiles).unwrap();
        let gen100 = profiles.join("home-manager-100-link");
        std::fs::create_dir_all(&gen100).unwrap();
        std::os::unix::fs::symlink(&gen100, profile_link(&user)).unwrap();
        let action = Action::SwitchGeneration {
            user: user.clone(),
            generation: Some(97),
            expect: Some(98),
        };
        let reporter: Arc<dyn ActivityReporter> = Arc::new(mix_core::NoopActivity);
        let files = mix_fs::Files::open(Path::new("/"), "r1").unwrap();
        let mut prepared = |_undo: &[Action]| Ok(());

        let outcome = perform(
            &action,
            None,
            &ProfileContext { mirror: None },
            &reporter,
            &mut Home::Local(&files),
            &Scope::root(),
            &mut prepared,
        )
        .await;

        assert_eq!(
            outcome,
            Some(Err(Failure::Conflict {
                subject: profile_link(&user).display().to_string(),
                expected: "generation 98".into(),
                found: "generation 100".into(),
            }))
        );
    }

    #[tokio::test]
    async fn deleting_the_active_generation_is_a_noop() {
        let home = tempfile::tempdir().unwrap();
        let user = user_in(home.path());
        let profiles = nix_profiles_dir(&user.home);
        std::fs::create_dir_all(&profiles).unwrap();
        let gen6 = profiles.join("home-manager-6-link");
        std::fs::create_dir_all(&gen6).unwrap();
        std::os::unix::fs::symlink(&gen6, profile_link(&user)).unwrap();

        let action = Action::DeleteGeneration {
            user: user.clone(),
            generation: 6,
        };
        let reporter: Arc<dyn ActivityReporter> = Arc::new(mix_core::NoopActivity);
        let context = ProfileContext { mirror: None };
        let scope = Scope::root();
        let mut prepared = |_undo: &[Action]| Ok(());
        let files = mix_fs::Files::open(Path::new("/"), "r1").unwrap();
        let mut home = Home::Local(&files);

        let outcome = perform(
            &action,
            None,
            &context,
            &reporter,
            &mut home,
            &scope,
            &mut prepared,
        )
        .await;
        assert!(matches!(outcome, Some(Ok(Performed { undo })) if undo.is_empty()));
    }
}
