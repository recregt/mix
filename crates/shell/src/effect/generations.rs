use std::path::{Path, PathBuf};
use std::sync::Arc;

use mix_core::ActivityReporter;
use mix_core::declared::identity::InvokingUser;
use mix_core::declared::paths::{
    DEFAULT_PROFILE_NIX_ENV, DEFAULT_PROFILE_NIX_STORE, GENERATION_LOCK_FILE,
    GENERATION_STATE_FILE, HOME_MANAGER_PROFILE_NAME, NIX_STORE, nix_profiles_dir,
};
use mix_core::effect::{Action, Fact, Failure, Outcome, Performed, ProfileFacts, Query};
use mix_exec::Scope;
use mix_nixgen::Pins;
use mix_nixgen::lock::LockedInput;

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
    clobbered_in(user, Path::new(NIX_STORE))
}

fn clobbered_in(user: &InvokingUser, store: &Path) -> Vec<PathBuf> {
    let Some(generation) = store_generation(&profile_link(user), store) else {
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

pub fn store_generation(link: &Path, store: &Path) -> Option<PathBuf> {
    let generation = std::fs::canonicalize(link).ok()?;
    (generation.parent() == Some(store)).then_some(generation)
}

pub fn store_input(generation: &Path, file: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::fd::OwnedFd;
    let nofollow =
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC;
    let dir = rustix::fs::open(
        generation,
        nofollow | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let found: OwnedFd =
        rustix::fs::openat(&dir, file, nofollow, rustix::fs::Mode::empty()).ok()?;
    if !rustix::fs::FileType::from_raw_mode(rustix::fs::fstat(&found).ok()?.st_mode).is_file() {
        return None;
    }
    let mut contents = Vec::new();
    std::fs::File::from(found).read_to_end(&mut contents).ok()?;
    Some(contents)
}

pub fn active_generation(user: &InvokingUser) -> Option<PathBuf> {
    store_generation(&profile_link(user), Path::new(NIX_STORE))
}

pub fn active_input(user: &InvokingUser, file: &str) -> Option<Vec<u8>> {
    store_input(&active_generation(user)?, file)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub number: u64,
    pub path: PathBuf,
    pub list: Option<String>,
    pub lock: Option<String>,
}

impl Active {
    pub fn pins(&self) -> Option<Pins<LockedInput>> {
        mix_core::declared::pins::read(self.lock.as_deref()?)
    }
}

impl From<&Active> for mix_events::v1::ActiveGeneration {
    fn from(active: &Active) -> Self {
        Self {
            number: active.number,
            store_path: active.path.display().to_string(),
        }
    }
}

fn numbered(user: &InvokingUser) -> Option<(u64, PathBuf)> {
    let link = profile_link(user);
    let target = std::fs::read_link(&link).ok()?;
    let number = generation_of(target.file_name()?.to_str()?)?;
    Some((number, link.parent()?.join(target)))
}

pub fn active(user: &InvokingUser) -> Option<Active> {
    active_in(user, Path::new(NIX_STORE))
}

fn active_in(user: &InvokingUser, store: &Path) -> Option<Active> {
    let (number, link) = numbered(user)?;
    let path = store_generation(&link, store)?;
    let text = |file: &str| {
        store_input(&path, file).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    Some(Active {
        number,
        list: text(GENERATION_STATE_FILE),
        lock: text(GENERATION_LOCK_FILE),
        path,
    })
}

fn current(user: &InvokingUser) -> Option<u64> {
    numbered(user).map(|(number, _)| number)
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
            if current(user) == Some(*generation) {
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
    source: mix_core::effect::FlakeSource,
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

    struct Profiles {
        root: tempfile::TempDir,
        store: PathBuf,
        user: InvokingUser,
    }

    impl Profiles {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let store = std::fs::canonicalize(root.path()).unwrap().join("store");
            std::fs::create_dir_all(&store).unwrap();
            let home = root.path().join("home");
            std::fs::create_dir_all(nix_profiles_dir(&home)).unwrap();
            let user = InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home,
            };
            Self { root, store, user }
        }

        fn built(&self, name: &str, inputs: &[(&str, &str)]) -> PathBuf {
            let generation = self.store.join(name);
            std::fs::create_dir_all(&generation).unwrap();
            for (file, text) in inputs {
                std::fs::write(generation.join(file), text).unwrap();
            }
            generation
        }

        fn point(&self, number: Option<u64>, target: &Path) {
            let profiles = nix_profiles_dir(&self.user.home);
            let link = profile_link(&self.user);
            let _ = std::fs::remove_file(&link);
            match number {
                Some(number) => {
                    let numbered = format!("{HOME_MANAGER_PROFILE_NAME}-{number}-link");
                    let _ = std::fs::remove_file(profiles.join(&numbered));
                    std::os::unix::fs::symlink(target, profiles.join(&numbered)).unwrap();
                    std::os::unix::fs::symlink(&numbered, &link).unwrap();
                }
                None => std::os::unix::fs::symlink(target, &link).unwrap(),
            }
        }
    }

    #[test]
    fn the_active_generation_is_its_number_store_path_list_and_lock_read_together() {
        let profiles = Profiles::new();
        let generation = profiles.built(
            "aaa-home-manager-generation",
            &[
                (GENERATION_STATE_FILE, "listed"),
                (GENERATION_LOCK_FILE, "locked"),
            ],
        );
        profiles.point(Some(3), &generation);

        let active = active_in(&profiles.user, &profiles.store).unwrap();

        assert_eq!(active.number, 3);
        assert_eq!(active.path, generation);
        assert_eq!(active.list.as_deref(), Some("listed"));
        assert_eq!(active.lock.as_deref(), Some("locked"));
        assert_eq!(current(&profiles.user), Some(3));
    }

    #[test]
    fn a_generation_without_its_inputs_is_still_active_but_holds_none() {
        let profiles = Profiles::new();
        let generation = profiles.built("aaa-home-manager-generation", &[]);
        profiles.point(Some(1), &generation);

        let active = active_in(&profiles.user, &profiles.store).unwrap();

        assert_eq!(active.list, None);
        assert_eq!(active.lock, None);
        assert_eq!(active.pins(), None);
    }

    #[test]
    fn a_generation_whose_store_path_is_gone_is_not_active_though_its_number_is_current() {
        let profiles = Profiles::new();
        profiles.point(
            Some(4),
            &profiles.store.join("gone-home-manager-generation"),
        );

        assert_eq!(active_in(&profiles.user, &profiles.store), None);
        assert_eq!(current(&profiles.user), Some(4));
    }

    #[test]
    fn a_profile_that_skips_the_numbered_link_has_no_active_generation() {
        let profiles = Profiles::new();
        let generation = profiles.built("aaa-home-manager-generation", &[]);
        profiles.point(None, &generation);

        assert_eq!(active_in(&profiles.user, &profiles.store), None);
        assert_eq!(current(&profiles.user), None);
    }

    #[test]
    fn a_generation_outside_the_store_is_not_active() {
        let profiles = Profiles::new();
        let elsewhere = profiles.root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        profiles.point(Some(2), &elsewhere);

        assert_eq!(active_in(&profiles.user, &profiles.store), None);
    }

    #[test]
    fn a_new_generation_replaces_the_old_as_the_active_one() {
        let profiles = Profiles::new();
        let first = profiles.built("aaa-home-manager-generation", &[]);
        let second = profiles.built("bbb-home-manager-generation", &[]);
        profiles.point(Some(1), &first);
        profiles.point(Some(2), &second);

        let active = active_in(&profiles.user, &profiles.store).unwrap();

        assert_eq!((active.number, active.path), (2, second));
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

        assert_eq!(active.pins(), Some(mix_nixgen::lock::EMBEDDED));
        assert_eq!(broken.pins(), None);
    }

    #[test]
    fn a_generation_is_read_only_from_a_regular_file_directly_in_the_store() {
        let root = tempfile::tempdir().unwrap();
        let store = std::fs::canonicalize(root.path()).unwrap().join("store");
        let generation = store.join("aaa-home-manager-generation");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("mix-state"), "listed").unwrap();
        std::fs::write(root.path().join("secret"), "private").unwrap();
        std::os::unix::fs::symlink(
            root.path().join("secret"),
            generation.join("mix-flake.lock"),
        )
        .unwrap();
        let link = root.path().join("profile");
        std::os::unix::fs::symlink(&generation, &link).unwrap();

        let found = store_generation(&link, &store).unwrap();

        assert_eq!(found, generation);
        assert_eq!(store_input(&found, "mix-state"), Some(b"listed".to_vec()));
        assert_eq!(store_input(&found, "mix-flake.lock"), None);
        assert_eq!(store_input(&found, "missing"), None);
    }

    #[test]
    fn a_profile_that_leads_outside_the_store_is_not_a_generation() {
        let root = tempfile::tempdir().unwrap();
        let store = std::fs::canonicalize(root.path()).unwrap().join("store");
        let elsewhere = root.path().join("elsewhere");
        let nested = store.join("aaa-generation").join("inner");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        for (name, target) in [("outside", &elsewhere), ("nested", &nested)] {
            let link = root.path().join(name);
            std::os::unix::fs::symlink(target, &link).unwrap();
            assert_eq!(store_generation(&link, &store), None, "{name}");
        }
        assert_eq!(
            store_generation(&root.path().join("dangling"), &store),
            None
        );
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

        assert_eq!(clobbered(&user), Vec::<PathBuf>::new());
        let store = std::fs::canonicalize(home.path().join("store")).unwrap();
        let found = clobbered_in(&user, &store);

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

        let outcome = perform(&action, None, &context, &reporter, &scope, &mut prepared).await;
        assert!(matches!(outcome, Some(Ok(Performed { undo })) if undo.is_empty()));
    }
}
