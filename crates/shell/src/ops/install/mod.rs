use mix_events::v1::InstallRequest;

use crate::Context;
use crate::profile::change;
use crate::request::{Concluded, Root};

pub(crate) async fn install(ctx: &Context, root: &mut Root, request: &InstallRequest) -> Concluded {
    if ctx.caller_is_root {
        return root.refuse(change::Error::NotRoot);
    }
    let Some(cfg) = ctx.user.as_ref() else {
        return root.refuse(change::Error::NotBootstrapped);
    };
    let mut performer = match change::begin(ctx, root).await {
        Ok(performer) => performer,
        Err(concluded) => return *concluded,
    };
    let settled = change::settled(&mut performer, cfg, &ctx.scope).await;
    let decided = match mix_core::change::install(&request.packages, settled) {
        Ok(decided) => decided,
        Err(refused) => return root.refuse(change::Error::from(refused)),
    };
    change::run(ctx, root, performer, cfg, change::Verb::Install, &decided).await
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use mix_core::identity::InvokingUser;
    use mix_core::paths::{HOME_NIX, STATE_FILE, mix_state_dir};
    use mix_core::state::StateManifest;

    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, InstallRequest, InstallResult, node_finished};

    use crate::request::ran::{Ran, ran};

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

    fn user_config(home: &std::path::Path) -> mix_core::targets::UserConfig {
        mix_core::targets::UserConfig {
            user: InvokingUser {
                uid: 1000,
                gid: 1000,
                name: "mix-user".to_string(),
                home: home.to_path_buf(),
            },
            flake: "flake-content".to_string(),
            lock: "lock-content".to_string(),
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
