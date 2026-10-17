use mix_core::effect::Failure;
use mix_core::ops::purge::{Settings, steps};
use mix_core::report::diagnose::{diagnostic, warning};
use mix_core::run::Verdict;
use mix_events::v1::{Code, PurgeResult, node_finished};
use mix_events::{Ending, ROOT};

use crate::Context;
use crate::ops::bootstrap::Error;
use crate::ops::removal::removal;
use crate::request::{Concluded, Root};

fn ending_of(verdict: &Verdict, settings: &Settings) -> Ending {
    match verdict {
        Verdict::Succeeded => {
            Ending::succeeded().with_result(node_finished::Result::Purge(PurgeResult {
                users: settings
                    .users
                    .iter()
                    .map(|user| user.name.clone())
                    .collect(),
            }))
        }
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    }
}

pub(crate) async fn purge(ctx: &Context, root: &mut Root) -> Concluded {
    if !ctx.host.is_root() {
        return root.refuse(Error::NotRoot("remove `mix`"));
    }
    let settings = Settings {
        users: ctx.host.enrolled(),
    };
    let verdict = match removal(ctx, root, steps(&settings)).await {
        Ok(verdict) => verdict,
        Err(refused) => return *refused,
    };
    if !ctx.dry_run && verdict == Verdict::Succeeded {
        for left in ctx.host.finish_removal() {
            let _ = root.tree.warn(ROOT, left_behind(&left));
        }
    }
    root.conclude(ending_of(&verdict, &settings))
}

fn left_behind(failure: &Failure) -> mix_events::v1::Diagnostic {
    let subject = match failure {
        Failure::Io { path, .. } => path.display().to_string(),
        _ => String::new(),
    };
    warning(
        Code::CleanupIncomplete,
        format!("could not remove {subject}"),
        failure,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::LOCK_FILE;
    use mix_core::model::World;
    use mix_core::model::testkit::account;
    use mix_events::v1::PurgeRequest;
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::ran::{caller, machine_of, ran};

    fn alice() -> InvokingUser {
        account("alice", 1000)
    }

    fn machine() -> Arc<Mutex<World>> {
        machine_of(&[alice()])
    }

    fn session(world: &Arc<Mutex<World>>) -> crate::Session {
        caller(world, true, Some(alice()))
    }

    fn as_caller(world: &Arc<Mutex<World>>, account: Option<InvokingUser>) -> crate::Session {
        caller(world, true, account)
    }

    fn users(ran: &crate::request::ran::Ran) -> Vec<String> {
        match ran.result() {
            Some(node_finished::Result::Purge(purged)) => purged.users.clone(),
            other => panic!("expected a removal result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_purge_names_the_users_it_cleaned_and_leaves_no_runtime() {
        let world = machine();
        let booted = ran(session(&world), Request::Bootstrap(Box::default())).await;
        assert_eq!(booted.code(), None);
        world
            .lock()
            .unwrap()
            .with_file(LOCK_FILE, b"", 0o644, (0, 0));

        let removed = ran(session(&world), Request::Purge(PurgeRequest {})).await;

        assert_eq!(removed.code(), None);
        assert!(removed.warnings().is_empty(), "{:?}", removed.warnings());
        assert_eq!(
            removed.result(),
            Some(&node_finished::Result::Purge(PurgeResult {
                users: vec!["alice".into()],
            }))
        );
        let world = world.lock().unwrap();
        for gone in ["/nix/store", "/etc/mix", LOCK_FILE] {
            assert!(!world.files.contains_key(Path::new(gone)), "{gone}");
        }
    }

    #[tokio::test]
    async fn a_dry_run_changes_nothing() {
        let world = machine();
        let booted = ran(session(&world), Request::Bootstrap(Box::default())).await;
        assert_eq!(booted.code(), None);
        let before = world.lock().unwrap().clone();

        let command = mix_events::v1::Command {
            dry_run: true,
            ..mix_events::command(Request::Purge(PurgeRequest {}))
        };
        let removed = crate::request::ran::ran_command(session(&world), command).await;

        assert_eq!(removed.code(), None);
        assert_eq!(*world.lock().unwrap(), before);
    }

    #[tokio::test]
    async fn a_machine_root_set_up_alone_is_removed_with_no_home_touched() {
        let world = machine();
        let booted = ran(as_caller(&world, None), Request::Bootstrap(Box::default())).await;
        assert_eq!(booted.code(), None);
        {
            let world = world.lock().unwrap();
            assert!(world.files.contains_key(Path::new("/nix/store")));
            assert!(world.groups["mix-users"].members.is_empty());
        }
        let home = world.lock().unwrap().files.clone();

        let removed = ran(as_caller(&world, None), Request::Purge(PurgeRequest {})).await;

        assert_eq!(removed.code(), None);
        assert_eq!(users(&removed), Vec::<String>::new());
        let world = world.lock().unwrap();
        assert!(!world.files.contains_key(Path::new("/nix/store")));
        let alice_before: Vec<_> = home
            .keys()
            .filter(|path| path.starts_with("/home/alice"))
            .collect();
        let alice_after: Vec<_> = world
            .files
            .keys()
            .filter(|path| path.starts_with("/home/alice"))
            .collect();
        assert_eq!(alice_before, alice_after);
    }

    #[tokio::test]
    async fn root_removes_the_homes_of_users_who_enrolled_after_a_root_setup() {
        let world = machine();
        let machine_only = ran(as_caller(&world, None), Request::Bootstrap(Box::default())).await;
        assert_eq!(machine_only.code(), None);
        let enrolled = ran(session(&world), Request::Bootstrap(Box::default())).await;
        assert_eq!(enrolled.code(), None);
        assert!(
            world
                .lock()
                .unwrap()
                .files
                .contains_key(&mix_core::declared::paths::mix_state_dir(&alice().home))
        );

        let removed = ran(as_caller(&world, None), Request::Purge(PurgeRequest {})).await;

        assert_eq!(removed.code(), None);
        assert_eq!(users(&removed), ["alice"]);
        let world = world.lock().unwrap();
        assert!(
            !world
                .files
                .contains_key(&mix_core::declared::paths::mix_state_dir(&alice().home))
        );
        assert!(!world.files.contains_key(Path::new("/nix/store")));
    }
}
