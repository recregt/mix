use mix_core::declared::identity::InvokingUser;
use mix_core::ops::user_remove::{Settings, steps};
use mix_core::report::diagnose::diagnostic;
use mix_core::run::Verdict;
use mix_events::Ending;
use mix_events::v1::{UserRemoveResult, node_finished};

use crate::Context;
use crate::ops::removal::removal;
use crate::profile::change::profile_user;
use crate::request::{Concluded, Root};

fn ending_of(verdict: &Verdict, ctx: &Context, user: &InvokingUser) -> Ending {
    match verdict {
        Verdict::Succeeded => {
            Ending::succeeded().with_result(node_finished::Result::UserRemove(UserRemoveResult {
                user: user.name.clone(),
                others: ctx
                    .host
                    .enrolled()
                    .into_iter()
                    .map(|enrolled| enrolled.name)
                    .filter(|name| *name != user.name)
                    .collect(),
            }))
        }
        Verdict::Failed { failure, .. } => Ending::failed(diagnostic(failure)),
        Verdict::Cancelled(cause) => Ending::cancelled(*cause),
    }
}

pub(crate) async fn user_remove(ctx: &Context, root: &mut Root) -> Concluded {
    let user = match profile_user(ctx) {
        Ok(config) => config.user.clone(),
        Err(error) => return root.refuse(error),
    };
    let settings = Settings { user: user.clone() };
    match removal(ctx, root, |_| (steps(&settings), ())).await {
        Ok((verdict, ())) => root.conclude(ending_of(&verdict, ctx, &user)),
        Err(refused) => *refused,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use mix_core::declared::identity::InvokingUser;
    use mix_core::declared::paths::mix_state_dir;
    use mix_core::model::World;
    use mix_core::model::testkit::account;
    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, UserRemoveRequest};

    use super::*;
    use crate::request::ran::{caller, machine_of, ran};

    fn machine() -> Arc<Mutex<World>> {
        machine_of(&[account("alice", 1000), account("bob", 1001)])
    }

    fn as_user(world: &Arc<Mutex<World>>, user: &InvokingUser) -> crate::Session {
        caller(world, false, Some(user.clone()))
    }

    fn as_root(world: &Arc<Mutex<World>>) -> crate::Session {
        caller(world, true, None)
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
            Request::UserRemove(UserRemoveRequest {}),
        )
        .await;

        assert_eq!(removed.code(), None);
        assert_eq!(
            removed.result(),
            Some(&node_finished::Result::UserRemove(UserRemoveResult {
                user: "alice".into(),
                others: vec!["bob".into()],
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
                Request::UserRemove(UserRemoveRequest {}),
            )
            .await;
            assert_eq!(removed.code(), None);
            if user.name == "bob" {
                assert_eq!(
                    removed.result(),
                    Some(&node_finished::Result::UserRemove(UserRemoveResult {
                        user: "bob".into(),
                        others: Vec::new(),
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

        let removed = ran(as_root(&world), Request::UserRemove(UserRemoveRequest {})).await;

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
            ..mix_events::command(Request::UserRemove(UserRemoveRequest {}))
        };
        let removed =
            crate::request::ran::ran_command(as_user(&world, &account("alice", 1000)), command)
                .await;

        assert_eq!(removed.code(), None);
        assert_eq!(*world.lock().unwrap(), before);
    }
}
