use crate::effect::{Action, Failure, GroupFacts, Owner, UserFacts, UserSpec};

pub trait Accounts {
    fn group(&self, name: &str) -> Option<GroupFacts>;
    fn user(&self, name: &str) -> Option<UserFacts>;
    fn group_with_gid(&self, gid: u32) -> Option<String>;
    fn user_with_uid(&self, uid: u32) -> Option<String>;
    fn primary_of(&self, gid: u32) -> Option<String>;
    fn groups_of(&self, user: &str) -> Vec<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accounted {
    Done,
    Go(Vec<Action>),
}

fn conflict(subject: &str, expected: impl Into<String>, found: impl Into<String>) -> Failure {
    Failure::Conflict {
        subject: subject.to_string(),
        expected: expected.into(),
        found: found.into(),
    }
}

fn expect_group(accounts: &impl Accounts, name: &str, gid: u32) -> Result<GroupFacts, Failure> {
    match accounts.group(name) {
        Some(found) if found.gid == gid => Ok(found),
        Some(found) => Err(conflict(
            name,
            format!("gid {gid}"),
            format!("gid {}", found.gid),
        )),
        None => Err(conflict(name, format!("gid {gid}"), "no group")),
    }
}

fn expect_user(accounts: &impl Accounts, name: &str, ids: Owner) -> Result<UserFacts, Failure> {
    match accounts.user(name) {
        Some(found) if (found.uid, found.gid) == ids => Ok(found),
        Some(found) => Err(conflict(
            name,
            format!("ids {ids:?}"),
            format!("ids {:?}", (found.uid, found.gid)),
        )),
        None => Err(conflict(name, format!("ids {ids:?}"), "no user")),
    }
}

fn gid_free(accounts: &impl Accounts, name: &str, gid: u32) -> Result<(), Failure> {
    match accounts.group_with_gid(gid) {
        Some(holder) if holder != name => Err(conflict(
            name,
            format!("gid {gid} free"),
            format!("gid {gid} held by {holder}"),
        )),
        _ => Ok(()),
    }
}

fn uid_free(accounts: &impl Accounts, name: &str, uid: u32) -> Result<(), Failure> {
    match accounts.user_with_uid(uid) {
        Some(holder) if holder != name => Err(conflict(
            name,
            format!("uid {uid} free"),
            format!("uid {uid} held by {holder}"),
        )),
        _ => Ok(()),
    }
}

fn members(accounts: &impl Accounts, group: &str) -> Vec<String> {
    accounts
        .group(group)
        .map(|group| group.members)
        .unwrap_or_default()
}

fn done_already(action: &Action, accounts: &impl Accounts) -> bool {
    match action {
        Action::AddGroup { name, gid } | Action::SetGroupGid { name, gid, .. } => {
            accounts.group(name).is_some_and(|found| found.gid == *gid)
        }
        Action::DeleteGroup { name, .. } => accounts.group(name).is_none(),
        Action::AddUser(spec) => accounts
            .user(&spec.name)
            .is_some_and(|found| (found.uid, found.gid) == (spec.uid, spec.gid)),
        Action::SetUserIds { name, ids, .. } => accounts
            .user(name)
            .is_some_and(|found| (found.uid, found.gid) == *ids),
        Action::DeleteUser { name, .. } => accounts.user(name).is_none(),
        Action::AddMember { group, user } => members(accounts, group).contains(user),
        Action::RemoveMember { group, user } => !members(accounts, group).contains(user),
        _ => false,
    }
}

pub fn account_precondition(
    action: &Action,
    accounts: &impl Accounts,
) -> Result<Accounted, Failure> {
    if done_already(action, accounts) {
        return Ok(Accounted::Done);
    }
    let undo = match action {
        Action::AddGroup { name, gid } => {
            if let Some(found) = accounts.group(name) {
                return Err(conflict(name, "no group", format!("gid {}", found.gid)));
            }
            gid_free(accounts, name, *gid)?;
            vec![Action::DeleteGroup {
                name: name.clone(),
                expect: *gid,
            }]
        }
        Action::SetGroupGid { name, gid, expect } => {
            expect_group(accounts, name, *expect)?;
            gid_free(accounts, name, *gid)?;
            vec![Action::SetGroupGid {
                name: name.clone(),
                gid: *expect,
                expect: *gid,
            }]
        }
        Action::DeleteGroup { name, expect } => {
            let found = expect_group(accounts, name, *expect)?;
            if let Some(user) = accounts.primary_of(*expect) {
                return Err(conflict(
                    name,
                    "no user in it as their primary group",
                    format!("the primary group of {user}"),
                ));
            }
            let mut undo = vec![Action::AddGroup {
                name: name.clone(),
                gid: *expect,
            }];
            undo.extend(found.members.into_iter().map(|user| Action::AddMember {
                group: name.clone(),
                user,
            }));
            undo
        }
        Action::AddUser(spec) => {
            if let Some(found) = accounts.user(&spec.name) {
                return Err(conflict(
                    &spec.name,
                    "no user",
                    format!("uid {}", found.uid),
                ));
            }
            uid_free(accounts, &spec.name, spec.uid)?;
            let missing = std::iter::once(spec.gid.to_string())
                .filter(|_| accounts.group_with_gid(spec.gid).is_none())
                .chain(
                    spec.groups
                        .iter()
                        .filter(|name| accounts.group(name).is_none())
                        .cloned(),
                )
                .next();
            if let Some(missing) = missing {
                return Err(conflict(&spec.name, format!("group {missing}"), "no group"));
            }
            vec![Action::DeleteUser {
                name: spec.name.clone(),
                expect: (spec.uid, spec.gid),
                comment: spec.comment.clone(),
            }]
        }
        Action::SetUserIds { name, ids, expect } => {
            expect_user(accounts, name, *expect)?;
            uid_free(accounts, name, ids.0)?;
            if accounts.group_with_gid(ids.1).is_none() {
                return Err(conflict(
                    name,
                    format!("a group with gid {}", ids.1),
                    "no such group",
                ));
            }
            vec![Action::SetUserIds {
                name: name.clone(),
                ids: *expect,
                expect: *ids,
            }]
        }
        Action::DeleteUser {
            name,
            expect,
            comment,
        } => {
            let found = expect_user(accounts, name, *expect)?;
            if found.comment != *comment {
                return Err(conflict(
                    name,
                    format!("comment {comment:?}"),
                    format!("comment {:?}", found.comment),
                ));
            }
            vec![Action::AddUser(UserSpec {
                name: name.clone(),
                uid: found.uid,
                gid: found.gid,
                home: found.home,
                shell: found.shell,
                comment: found.comment,
                groups: accounts.groups_of(name),
            })]
        }
        Action::AddMember { group, user } => {
            if accounts.group(group).is_none() {
                return Err(conflict(group, "a group", "no group"));
            }
            if accounts.user(user).is_none() {
                return Err(conflict(group, format!("user {user}"), "no user"));
            }
            vec![Action::RemoveMember {
                group: group.clone(),
                user: user.clone(),
            }]
        }
        Action::RemoveMember { group, user } => {
            if accounts.group(group).is_none() {
                return Err(conflict(group, "a group", "no group"));
            }
            vec![Action::AddMember {
                group: group.clone(),
                user: user.clone(),
            }]
        }
        _ => Vec::new(),
    };
    Ok(Accounted::Go(undo))
}
