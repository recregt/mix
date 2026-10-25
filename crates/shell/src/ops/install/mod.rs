use mix_events::v1::InstallRequest;

use crate::Context;
use crate::profile::change;
use crate::request::{Concluded, Root};

pub(crate) async fn install(ctx: &Context, root: &mut Root, request: &InstallRequest) -> Concluded {
    let cfg = match change::profile_user(ctx) {
        Ok(cfg) => cfg,
        Err(error) => return root.refuse(error),
    };
    let (mut performer, cfg) = match change::begin(ctx, root, cfg).await {
        Ok(begun) => begun,
        Err(concluded) => return *concluded,
    };
    let cfg = &cfg;
    let settled = change::settled(&mut performer, cfg, &ctx.scope).await;
    let decided = match mix_core::ops::change::install(&request.packages, settled) {
        Ok(decided) => decided,
        Err(refused) => return root.refuse(change::Error::from(refused)),
    };
    if ctx.dry_run
        && !decided.changed.is_empty()
        && let Err(error) = ctx
            .upstream
            .check(&cfg.user, &decided.changed, ctx.mirror(), &ctx.scope)
            .await
    {
        return root.refuse(change::Error::Core(error));
    }
    change::run(ctx, root, performer, cfg, change::Verb::Install, &decided).await
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::declared::state::StateManifest;

    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, InstallRequest, InstallResult, node_finished};

    use std::sync::{Arc, Mutex};

    use mix_core::ops::update::Versions;
    use mix_nixgen::Pins;
    use mix_nixgen::lock::LockedInput;

    use crate::request::ran::{Ran, ran, ran_command};
    use crate::request::upstream::{Checked, Evaluated, Newest, Upstream};

    const MISSING: &str = include_str!("../../../fixtures/nix/eval-missing.txt");

    #[derive(Default)]
    struct Rejecting {
        asked: Mutex<Vec<Vec<String>>>,
    }

    impl Upstream for Rejecting {
        fn newest<'a>(&'a self, _: &'a InvokingUser, _: &'a mix_exec::Scope) -> Newest<'a> {
            unreachable!("an install never asks for the newest pins")
        }

        fn check<'a>(
            &'a self,
            _: &'a InvokingUser,
            packages: &'a [String],
            _: Option<&'a str>,
            _: &'a mix_exec::Scope,
        ) -> Checked<'a> {
            self.asked.lock().unwrap().push(packages.to_vec());
            Box::pin(async {
                Err(mix_core::Error::Command {
                    command: "nix eval".to_string(),
                    detail: MISSING.to_string(),
                })
            })
        }

        fn versions<'a>(
            &'a self,
            _: &'a InvokingUser,
            _: &'a [String],
            _: &'a Pins<LockedInput>,
            _: &'a mix_exec::Scope,
        ) -> Evaluated<'a> {
            Box::pin(async { Ok(Versions::new()) })
        }
    }

    async fn dry_run(home: &std::path::Path, upstream: Arc<Rejecting>, packages: &[&str]) -> Ran {
        let packages = packages.iter().map(|package| package.to_string()).collect();
        let mut command = mix_events::command(Request::Install(InstallRequest { packages }));
        command.dry_run = true;
        ran_command(context(home).with_upstream(upstream), command).await
    }

    #[tokio::test]
    async fn a_dry_run_install_fails_the_way_the_install_would_for_a_package_nix_does_not_know() {
        let home = seeded_home();
        let state_before =
            std::fs::read_to_string(mix_state_dir(home.path()).join(STATE_FILE)).unwrap();
        let upstream = Arc::new(Rejecting::default());

        let ran = dry_run(home.path(), Arc::clone(&upstream), &["git", "make"]).await;

        assert_eq!(ran.code(), Some(Code::UnknownPackage));
        assert_eq!(*upstream.asked.lock().unwrap(), [vec!["make".to_string()]]);
        assert_eq!(
            std::fs::read_to_string(mix_state_dir(home.path()).join(STATE_FILE)).unwrap(),
            state_before
        );
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn a_dry_run_install_of_packages_already_there_asks_nix_nothing() {
        let home = seeded_home();
        let upstream = Arc::new(Rejecting::default());

        let ran = dry_run(home.path(), Arc::clone(&upstream), &["git"]).await;

        assert_eq!(installed(&ran).skipped, vec!["git".to_string()]);
        assert!(upstream.asked.lock().unwrap().is_empty());
    }

    fn seeded_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mix_state_dir(home.path())).unwrap();
        std::fs::write(
            mix_state_dir(home.path()).join(STATE_FILE),
            StateManifest::seed().render(),
        )
        .unwrap();
        home
    }

    fn context(home: &std::path::Path) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_user(Some(user_config(home)))
            .with_journals(home.join("journal"))
    }

    fn user_config(home: &std::path::Path) -> mix_core::declared::targets::UserConfig {
        mix_core::declared::targets::UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            pins: mix_nixgen::lock::EMBEDDED,
            flake: "flake-content".to_string(),
            lock: mix_nixgen::lock::render(mix_nixgen::lock::EMBEDDED),
            home: "home-content".to_string(),
            restored_state: None,
        }
    }

    async fn install(home: &std::path::Path, packages: &[&str]) -> Ran {
        let packages = packages.iter().map(|package| package.to_string()).collect();
        ran(context(home), Request::Install(InstallRequest { packages })).await
    }

    fn installed(ran: &Ran) -> &InstallResult {
        match ran.result() {
            Some(node_finished::Result::Install(installed)) => installed,
            other => panic!("expected an install result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn install_skips_a_package_that_is_already_present() {
        let home = seeded_home();

        let ran = install(home.path(), &["git"]).await;

        assert!(installed(&ran).added.is_empty());
        assert_eq!(installed(&ran).skipped, vec!["git".to_string()]);
    }

    #[tokio::test]
    async fn install_touches_nothing_when_every_package_is_already_present() {
        let home = seeded_home();
        let state_before =
            std::fs::read_to_string(mix_state_dir(home.path()).join(STATE_FILE)).unwrap();

        install(home.path(), &["git"]).await;

        assert_eq!(
            std::fs::read_to_string(mix_state_dir(home.path()).join(STATE_FILE)).unwrap(),
            state_before
        );
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }

    #[tokio::test]
    async fn install_reports_a_repeated_package_once() {
        let home = seeded_home();

        let ran = install(home.path(), &["git", "git"]).await;

        assert_eq!(installed(&ran).skipped, vec!["git".to_string()]);
        assert!(installed(&ran).added.is_empty());
    }

    #[tokio::test]
    async fn install_leaves_nothing_written_for_an_invalid_package_name() {
        let home = seeded_home();

        let ran = install(home.path(), &["not a valid ident"]).await;

        assert_eq!(ran.code(), Some(Code::InvalidPackage));
        assert!(!mix_state_dir(home.path()).join(HOME_NIX).exists());
    }
}
