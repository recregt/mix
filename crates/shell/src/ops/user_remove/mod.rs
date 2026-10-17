use mix_core::declared::identity::InvokingUser;
use mix_core::ops::self_remove::{Settings, steps};
use mix_core::report::diagnose::{diagnostic, warning};
use mix_core::run::{Runner, Verdict};
use mix_events::v1::{Code, SelfRemoveResult, node_finished};
use mix_events::{Ending, ROOT};

use crate::Context;
use crate::drive::drive;
use crate::ops::bootstrap::{Error, error_from};
use crate::profile::change::profile_user;
use crate::request::{Concluded, Root};

fn ending_of(verdict: &Verdict, user: &InvokingUser, others: Vec<String>) -> Ending {
    match verdict {
        Verdict::Succeeded => {
            Ending::succeeded().with_result(node_finished::Result::SelfRemove(SelfRemoveResult {
                users: vec![user.name.clone()],
                others,
                machine: false,
            }))
        }
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    }
}

fn others_than(ctx: &Context, user: &InvokingUser) -> Vec<String> {
    ctx.host
        .enrolled()
        .into_iter()
        .map(|enrolled| enrolled.name)
        .filter(|name| *name != user.name)
        .collect()
}

pub(crate) async fn self_remove(ctx: &Context, root: &mut Root) -> Concluded {
    let user = match profile_user(ctx) {
        Ok(config) => config.user.clone(),
        Err(error) => return root.refuse(error),
    };
    let mut performer = match ctx.performer() {
        Ok(performer) => performer,
        Err(source) => {
            return root.refuse(Error::Core(mix_core::Error::Io {
                path: "/".into(),
                source,
            }));
        }
    };
    let settings = Settings { user: user.clone() };
    let scope = &ctx.scope;
    crate::ops::recover_interrupted(ctx, &mut root.tree, &mut performer, true).await;
    let mut runner = Runner::new(ROOT, steps(&settings));
    if ctx.dry_run {
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
        let others = others_than(ctx, &user);
        return root.conclude(ending_of(&report.verdict, &user, others));
    }
    let mut journal = match ctx.journal(ctx.journals.as_path()) {
        Ok(journal) => journal,
        Err(failure) => return root.refuse(error_from(failure)),
    };
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
        return root.conclude(ending_of(&report.verdict, &user, Vec::new()));
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
    let others = others_than(ctx, &user);
    root.conclude(ending_of(&report.verdict, &user, others))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::{RUNNING_PROGRAM, mix_state_dir};
    use mix_core::model::World;
    use mix_events::v1::SelfRemoveRequest;
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::context::Host;
    use crate::request::ran::ran;

    fn account(name: &str, uid: u32) -> InvokingUser {
        InvokingUser {
            uid,
            gid: uid,
            name: name.into(),
            home: format!("/home/{name}").into(),
        }
    }

    fn machine() -> Arc<Mutex<World>> {
        let mut world = World::default();
        for user in [account("alice", 1000), account("bob", 1001)] {
            world.with_dir(&user.home, 0o700, (user.uid, user.gid));
            world.users.insert(
                user.name.clone(),
                mix_core::effect::UserFacts {
                    uid: user.uid,
                    gid: user.gid,
                    home: user.home.clone(),
                    shell: "/bin/sh".into(),
                    comment: String::new(),
                },
            );
        }
        world.with_file(RUNNING_PROGRAM, b"mix-daemon", 0o755, (0, 0));
        Arc::new(Mutex::new(world))
    }

    fn as_user(world: &Arc<Mutex<World>>, user: &InvokingUser) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_host(Host::Model(Arc::clone(world)))
            .with_caller(crate::Caller::Account {
                peer_is_root: false,
                account: Some(user.clone()),
            })
    }

    fn as_root(world: &Arc<Mutex<World>>) -> crate::Session {
        crate::Session::new(mix_exec::Scope::root())
            .with_host(Host::Model(Arc::clone(world)))
            .with_caller(crate::Caller::Account {
                peer_is_root: true,
                account: None,
            })
    }

    async fn enrolled(world: &Arc<Mutex<World>>) {
        for user in [account("alice", 1000), account("bob", 1001)] {
            let booted = ran(as_user(world, &user), Request::Bootstrap(Box::default())).await;
            assert_eq!(booted.code(), None);
        }
    }

    #[tokio::test]
    async fn a_user_leaves_alone_and_the_machine_and_the_other_users_stay() {
        let world = machine();
        enrolled(&world).await;

        let removed = ran(
            as_user(&world, &account("alice", 1000)),
            Request::SelfRemove(SelfRemoveRequest {}),
        )
        .await;

        assert_eq!(removed.code(), None);
        assert_eq!(
            removed.result(),
            Some(&node_finished::Result::SelfRemove(SelfRemoveResult {
                users: vec!["alice".into()],
                others: vec!["bob".into()],
                machine: false,
            }))
        );
        let world = world.lock().unwrap();
        assert!(
            !world
                .files
                .contains_key(&mix_state_dir(&account("alice", 1000).home))
        );
        assert!(
            world
                .files
                .contains_key(&mix_state_dir(&account("bob", 1001).home))
        );
        assert!(world.files.contains_key(Path::new("/nix/store")));
        assert_eq!(world.groups["mix-users"].members, ["bob"]);
    }

    #[tokio::test]
    async fn the_last_user_to_leave_sees_that_nobody_else_remains() {
        let world = machine();
        enrolled(&world).await;
        for user in [account("alice", 1000), account("bob", 1001)] {
            let removed = ran(
                as_user(&world, &user),
                Request::SelfRemove(SelfRemoveRequest {}),
            )
            .await;
            assert_eq!(removed.code(), None);
            if user.name == "bob" {
                assert_eq!(
                    removed.result(),
                    Some(&node_finished::Result::SelfRemove(SelfRemoveResult {
                        users: vec!["bob".into()],
                        others: Vec::new(),
                        machine: false,
                    }))
                );
            }
        }
        assert!(
            world
                .lock()
                .unwrap()
                .files
                .contains_key(Path::new("/nix/store"))
        );
    }

    #[tokio::test]
    async fn root_has_no_account_to_remove() {
        let world = machine();
        enrolled(&world).await;
        let before = world.lock().unwrap().clone();

        let removed = ran(as_root(&world), Request::SelfRemove(SelfRemoveRequest {})).await;

        assert_eq!(removed.code(), Some(Code::RootNotAllowed));
        assert_eq!(*world.lock().unwrap(), before);
    }

    #[tokio::test]
    async fn a_dry_run_changes_nothing() {
        let world = machine();
        enrolled(&world).await;
        let before = world.lock().unwrap().clone();

        let command = mix_events::v1::Command {
            dry_run: true,
            ..mix_events::command(Request::SelfRemove(SelfRemoveRequest {}))
        };
        let removed =
            crate::request::ran::ran_command(as_user(&world, &account("alice", 1000)), command)
                .await;

        assert_eq!(removed.code(), None);
        assert_eq!(*world.lock().unwrap(), before);
    }
}
