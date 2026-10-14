use mix_core::effect::Failure;
use mix_core::ops::self_remove::{Settings, steps};
use mix_core::report::diagnose::{diagnostic, warning};
use mix_core::run::{Runner, Verdict};
use mix_events::v1::{Code, SelfRemoveResult, node_finished};
use mix_events::{Ending, ROOT};

use crate::Context;
use crate::drive::drive;
use crate::ops::bootstrap::{Error, error_from};
use crate::request::{Concluded, Root};

fn ending_of(verdict: &Verdict, settings: &Settings) -> Ending {
    match verdict {
        Verdict::Succeeded => {
            Ending::succeeded().with_result(node_finished::Result::SelfRemove(SelfRemoveResult {
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

pub(crate) async fn self_remove(ctx: &Context, root: &mut Root) -> Concluded {
    if !ctx.host.is_root() {
        return root.refuse(Error::NotRoot("remove `mix`"));
    }
    let mut performer = match ctx.performer() {
        Ok(performer) => performer,
        Err(source) => {
            return root.refuse(Error::Core(mix_core::Error::Io {
                path: "/".into(),
                source,
            }));
        }
    };
    let settings = Settings {
        users: ctx.host.enrolled(),
    };
    let scope = &ctx.scope;
    crate::ops::recover_interrupted(ctx, &mut root.tree, &mut performer, true).await;
    if ctx.dry_run {
        let mut runner = Runner::new(ROOT, steps(&settings));
        let report = drive(
            &mut runner,
            &mut root.tree,
            &mut performer,
            scope,
            &root.stopped,
            &mut Vec::new(),
            &mut ctx.relay(),
        )
        .await;
        return root.conclude(ending_of(&report.verdict, &settings));
    }
    let mut journal = match ctx.journal(ctx.journals.as_path()) {
        Ok(journal) => journal,
        Err(failure) => return root.refuse(error_from(failure)),
    };
    let mut runner = Runner::new(ROOT, steps(&settings));
    let report = drive(
        &mut runner,
        &mut root.tree,
        &mut performer,
        scope,
        &root.stopped,
        &mut journal,
        &mut ctx.relay(),
    )
    .await;
    if !report.rollback_failures.is_empty() {
        return root.conclude(ending_of(&report.verdict, &settings));
    }
    if let Err(failure) = journal.finish() {
        let _ = root.tree.warn(
            ROOT,
            warning(
                Code::CleanupIncomplete,
                "could not remove the finished journal",
                &failure,
            ),
        );
    }
    if report.verdict == Verdict::Succeeded {
        for left in ctx.host.finish_removal() {
            let _ = root.tree.warn(ROOT, left_behind(&left));
        }
    }
    root.conclude(ending_of(&report.verdict, &settings))
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
    use mix_core::declared::paths::{LOCK_FILE, RUNNING_PROGRAM};
    use mix_core::model::World;
    use mix_events::v1::SelfRemoveRequest;
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::context::Host;
    use crate::request::ran::ran;

    fn alice() -> InvokingUser {
        InvokingUser {
            uid: 1000,
            gid: 1000,
            name: "alice".into(),
            home: "/home/alice".into(),
        }
    }

    fn machine() -> Arc<Mutex<World>> {
        let mut world = World::default();
        world.with_dir("/home/alice", 0o700, (1000, 1000));
        world.users.insert(
            "alice".into(),
            mix_core::effect::UserFacts {
                uid: 1000,
                gid: 1000,
                home: "/home/alice".into(),
                shell: "/bin/sh".into(),
                comment: String::new(),
            },
        );
        world.with_file(RUNNING_PROGRAM, b"mix-daemon", 0o755, (0, 0));
        Arc::new(Mutex::new(world))
    }

    fn session(world: &Arc<Mutex<World>>) -> crate::Session {
        as_caller(world, Some(alice()))
    }

    fn as_caller(world: &Arc<Mutex<World>>, account: Option<InvokingUser>) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_host(Host::Model(Arc::clone(world)))
            .with_caller(crate::Caller::Account {
                peer_is_root: true,
                account,
            })
    }

    fn users(ran: &crate::request::ran::Ran) -> Vec<String> {
        match ran.result() {
            Some(node_finished::Result::SelfRemove(removed)) => removed.users.clone(),
            other => panic!("expected a removal result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_removal_names_the_users_it_cleaned_and_leaves_no_runtime() {
        let world = machine();
        let booted = ran(session(&world), Request::Bootstrap(Box::default())).await;
        assert_eq!(booted.code(), None);
        world
            .lock()
            .unwrap()
            .with_file(LOCK_FILE, b"", 0o644, (0, 0));

        let removed = ran(session(&world), Request::SelfRemove(SelfRemoveRequest {})).await;

        assert_eq!(removed.code(), None);
        assert!(removed.warnings().is_empty(), "{:?}", removed.warnings());
        assert_eq!(
            removed.result(),
            Some(&node_finished::Result::SelfRemove(SelfRemoveResult {
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
            ..mix_events::command(Request::SelfRemove(SelfRemoveRequest {}))
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

        let removed = ran(
            as_caller(&world, None),
            Request::SelfRemove(SelfRemoveRequest {}),
        )
        .await;

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

        let removed = ran(
            as_caller(&world, None),
            Request::SelfRemove(SelfRemoveRequest {}),
        )
        .await;

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
