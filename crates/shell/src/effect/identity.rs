use mix_core::effect::{
    Action, Fact, Failure, GroupFacts, Outcome, Owner, Performed, Query, UserFacts, UserSpec,
};
use mix_exec::Scope;

use crate::effect::files::Prepared;
use crate::effect::tools::root_command;

pub fn observe(query: &Query) -> Option<Fact> {
    Some(match query {
        Query::Group(name) => Fact::Group(group(name)),
        Query::User(name) => Fact::User(user(name)),
        _ => return None,
    })
}

fn group(name: &str) -> Option<GroupFacts> {
    let group = nix::unistd::Group::from_name(name).ok().flatten()?;
    Some(GroupFacts {
        gid: group.gid.as_raw(),
        members: group.mem,
    })
}

fn user(name: &str) -> Option<UserFacts> {
    let user = nix::unistd::User::from_name(name).ok().flatten()?;
    Some(UserFacts {
        uid: user.uid.as_raw(),
        gid: user.gid.as_raw(),
        home: user.dir,
        shell: user.shell,
        comment: user.gecos.to_string_lossy().into_owned(),
    })
}

fn user_by_uid(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.name)
}

fn gid_exists(gid: u32) -> bool {
    nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))
        .ok()
        .flatten()
        .is_some()
}

fn conflict(subject: &str, expected: impl Into<String>, found: impl Into<String>) -> Failure {
    Failure::Conflict {
        subject: subject.to_string(),
        expected: expected.into(),
        found: found.into(),
    }
}

pub fn command(action: &Action) -> Option<(&'static str, Vec<String>)> {
    Some(match action {
        Action::AddGroup { name, gid } => (
            "groupadd",
            vec![
                "--system".into(),
                "--gid".into(),
                gid.to_string(),
                name.clone(),
            ],
        ),
        Action::SetGroupGid { name, gid, .. } => (
            "groupmod",
            vec!["--gid".into(), gid.to_string(), name.clone()],
        ),
        Action::DeleteGroup { name, .. } => ("groupdel", vec![name.clone()]),
        Action::AddUser(spec) => {
            let mut args = vec![
                "--system".to_string(),
                "--no-create-home".into(),
                "--no-user-group".into(),
                "--home-dir".into(),
                spec.home.display().to_string(),
                "--shell".into(),
                spec.shell.display().to_string(),
                "--uid".into(),
                spec.uid.to_string(),
                "--gid".into(),
                spec.gid.to_string(),
                "--comment".into(),
                spec.comment.clone(),
            ];
            if !spec.groups.is_empty() {
                args.push("--groups".into());
                args.push(spec.groups.join(","));
            }
            args.push(spec.name.clone());
            ("useradd", args)
        }
        Action::SetUserIds { name, ids, .. } => (
            "usermod",
            vec![
                "--uid".into(),
                ids.0.to_string(),
                "--gid".into(),
                ids.1.to_string(),
                name.clone(),
            ],
        ),
        Action::DeleteUser { name, .. } => ("userdel", vec![name.clone()]),
        Action::AddMember { group, user } => {
            ("gpasswd", vec!["--add".into(), user.clone(), group.clone()])
        }
        Action::RemoveMember { group, user } => (
            "gpasswd",
            vec!["--delete".into(), user.clone(), group.clone()],
        ),
        _ => return None,
    })
}

pub fn classify(
    tool: &str,
    subject: &str,
    status: Option<i32>,
    stderr: &[u8],
) -> Result<(), Failure> {
    let conflict = |expected: &str, found: &str| Err(conflict(subject, expected, found));
    match (tool, status) {
        (_, Some(0)) => Ok(()),
        ("useradd" | "usermod", Some(4)) => conflict("a free uid", "the uid in use"),
        ("groupadd" | "groupmod", Some(4)) => conflict("a free gid", "the gid in use"),
        ("useradd" | "groupadd" | "groupmod", Some(9)) => {
            conflict("a free name", "the name in use")
        }
        ("usermod" | "userdel" | "groupmod" | "groupdel", Some(6)) => {
            conflict("an existing account", "no such account")
        }
        ("userdel", Some(8)) => conflict("a user nobody is logged in as", "a logged-in user"),
        (_, status) => Err(Failure::CommandFailed {
            program: tool.to_string(),
            status,
            output_tail: String::from_utf8_lossy(stderr).trim().to_string(),
        }),
    }
}

fn subject(action: &Action) -> String {
    match action {
        Action::AddGroup { name, .. }
        | Action::SetGroupGid { name, .. }
        | Action::DeleteGroup { name, .. }
        | Action::SetUserIds { name, .. }
        | Action::DeleteUser { name, .. } => name.clone(),
        Action::AddUser(spec) => spec.name.clone(),
        Action::AddMember { group, user } | Action::RemoveMember { group, user } => {
            format!("{user} in {group}")
        }
        _ => String::new(),
    }
}

pub fn already(action: &Action) -> bool {
    let members = |name: &str| group(name).map(|group| group.members).unwrap_or_default();
    match action {
        Action::AddGroup { name, gid } | Action::SetGroupGid { name, gid, .. } => {
            group(name).is_some_and(|found| found.gid == *gid)
        }
        Action::DeleteGroup { name, .. } => group(name).is_none(),
        Action::AddUser(spec) => {
            user(&spec.name).is_some_and(|found| (found.uid, found.gid) == (spec.uid, spec.gid))
        }
        Action::SetUserIds { name, ids, .. } => {
            user(name).is_some_and(|found| (found.uid, found.gid) == *ids)
        }
        Action::DeleteUser { name, .. } => user(name).is_none(),
        Action::AddMember { group: name, user } => members(name).contains(user),
        Action::RemoveMember { group: name, user } => !members(name).contains(user),
        _ => false,
    }
}

fn precondition(action: &Action) -> Result<Vec<Action>, Failure> {
    let expect_group = |name: &str, gid: u32| match group(name) {
        Some(found) if found.gid == gid => Ok(found),
        Some(found) => Err(conflict(
            name,
            format!("gid {gid}"),
            format!("gid {}", found.gid),
        )),
        None => Err(conflict(name, format!("gid {gid}"), "no group")),
    };
    let expect_user = |name: &str, ids: Owner| match user(name) {
        Some(found) if (found.uid, found.gid) == ids => Ok(found),
        Some(found) => Err(conflict(
            name,
            format!("ids {ids:?}"),
            format!("ids {:?}", (found.uid, found.gid)),
        )),
        None => Err(conflict(name, format!("ids {ids:?}"), "no user")),
    };
    let members = |name: &str| group(name).map(|group| group.members).unwrap_or_default();
    Ok(match action {
        Action::AddGroup { name, gid } => {
            if let Some(found) = group(name) {
                return Err(conflict(name, "no group", format!("gid {}", found.gid)));
            }
            vec![Action::DeleteGroup {
                name: name.clone(),
                expect: *gid,
            }]
        }
        Action::SetGroupGid { name, gid, expect } => {
            expect_group(name, *expect)?;
            vec![Action::SetGroupGid {
                name: name.clone(),
                gid: *expect,
                expect: *gid,
            }]
        }
        Action::DeleteGroup { name, expect } => {
            let found = expect_group(name, *expect)?;
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
            if let Some(found) = user(&spec.name) {
                return Err(conflict(
                    &spec.name,
                    "no user",
                    format!("uid {}", found.uid),
                ));
            }
            if let Some(holder) = user_by_uid(spec.uid) {
                return Err(conflict(
                    &spec.name,
                    format!("uid {} free", spec.uid),
                    format!("uid {} held by {holder}", spec.uid),
                ));
            }
            let missing = std::iter::once(spec.gid.to_string())
                .filter(|_| !gid_exists(spec.gid))
                .chain(
                    spec.groups
                        .iter()
                        .filter(|name| group(name).is_none())
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
            expect_user(name, *expect)?;
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
            let found = expect_user(name, *expect)?;
            if found.comment != *comment {
                return Err(conflict(
                    name,
                    format!("comment {comment:?}"),
                    format!("comment {:?}", found.comment),
                ));
            }
            let groups = supplementary_groups(name);
            vec![Action::AddUser(UserSpec {
                name: name.clone(),
                uid: found.uid,
                gid: found.gid,
                home: found.home,
                shell: found.shell,
                comment: found.comment,
                groups,
            })]
        }
        Action::AddMember {
            group: name,
            user: member,
        } => {
            let Some(found) = group(name) else {
                return Err(conflict(name, "a group", "no group"));
            };
            if found.members.contains(member) {
                return Err(conflict(
                    name,
                    format!("{member} absent"),
                    format!("{member} present"),
                ));
            }
            vec![Action::RemoveMember {
                group: name.clone(),
                user: member.clone(),
            }]
        }
        Action::RemoveMember { group: name, user } => {
            if !members(name).contains(user) {
                return Err(conflict(
                    name,
                    format!("{user} present"),
                    format!("{user} absent"),
                ));
            }
            vec![Action::AddMember {
                group: name.clone(),
                user: user.clone(),
            }]
        }
        _ => Vec::new(),
    })
}

fn supplementary_groups(user: &str) -> Vec<String> {
    let Ok(contents) = std::fs::read_to_string("/etc/group") else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let members = fields.nth(2)?;
            members
                .split(',')
                .any(|member| member == user)
                .then(|| name.to_string())
        })
        .collect()
}

async fn stop_processes(user: &str, scope: &Scope) -> Result<(), Failure> {
    let output = root_command("pkill")?
        .args(["--signal", "KILL", "--uid", user])
        .output(scope)
        .await
        .map_err(|error| match error {
            mix_exec::Error::Cancelled { .. } => Failure::Cancelled,
            _ => Failure::SpawnFailed {
                program: "pkill".to_string(),
                kind: std::io::ErrorKind::Other,
            },
        })?;
    match output.status.code() {
        Some(0 | 1) => Ok(()),
        status => Err(Failure::CommandFailed {
            program: "pkill".to_string(),
            status,
            output_tail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }),
    }
}

pub async fn perform(
    action: &Action,
    scope: &Scope,
    prepared: &mut Prepared<'_>,
) -> Option<Outcome> {
    let (tool, args) = command(action)?;
    Some(
        async {
            if already(action) {
                return Ok(Performed { undo: Vec::new() });
            }
            let undo = precondition(action)?;
            prepared(&undo)?;
            if let Action::DeleteUser { name, .. } = action {
                stop_processes(name, scope).await?;
            }
            let output = root_command(tool)?
                .args(&args)
                .output(scope)
                .await
                .map_err(|error| match error {
                    mix_exec::Error::Cancelled { .. } => Failure::Cancelled,
                    other => Failure::SpawnFailed {
                        program: tool.to_string(),
                        kind: match other {
                            mix_exec::Error::Spawn { source, .. } => source.kind(),
                            _ => std::io::ErrorKind::Other,
                        },
                    },
                })?;
            classify(tool, &subject(action), output.status.code(), &output.stderr)?;
            Ok(Performed { undo })
        }
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_user_is_added_with_every_detail_on_the_command_line() {
        let (tool, args) = command(&Action::AddUser(UserSpec {
            name: "nixbld1".into(),
            uid: 30_001,
            gid: 30_000,
            home: "/var/empty".into(),
            shell: "/usr/sbin/nologin".into(),
            comment: "mix build user 1".into(),
            groups: vec!["nixbld".into()],
        }))
        .unwrap();

        assert_eq!(tool, "useradd");
        assert_eq!(
            args,
            [
                "--system",
                "--no-create-home",
                "--no-user-group",
                "--home-dir",
                "/var/empty",
                "--shell",
                "/usr/sbin/nologin",
                "--uid",
                "30001",
                "--gid",
                "30000",
                "--comment",
                "mix build user 1",
                "--groups",
                "nixbld",
                "nixbld1"
            ]
        );
    }

    #[test]
    fn membership_is_changed_with_gpasswd() {
        assert_eq!(
            command(&Action::AddMember {
                group: "mix-users".into(),
                user: "alice".into()
            }),
            Some((
                "gpasswd",
                vec!["--add".into(), "alice".into(), "mix-users".into()]
            ))
        );
        assert_eq!(
            command(&Action::RemoveMember {
                group: "mix-users".into(),
                user: "alice".into()
            }),
            Some((
                "gpasswd",
                vec!["--delete".into(), "alice".into(), "mix-users".into()]
            ))
        );
    }

    #[test]
    fn documented_exit_codes_are_conflicts_not_failed_commands() {
        for (tool, status) in [
            ("useradd", 4),
            ("useradd", 9),
            ("groupadd", 4),
            ("groupadd", 9),
            ("groupmod", 6),
            ("userdel", 6),
            ("userdel", 8),
        ] {
            assert!(
                matches!(
                    classify(tool, "x", Some(status), b""),
                    Err(Failure::Conflict { .. })
                ),
                "{tool} {status}"
            );
        }
    }

    #[test]
    fn an_undocumented_exit_keeps_what_the_tool_said() {
        assert_eq!(
            classify(
                "groupadd",
                "nixbld",
                Some(10),
                b"groupadd: cannot lock /etc/group\n"
            ),
            Err(Failure::CommandFailed {
                program: "groupadd".into(),
                status: Some(10),
                output_tail: "groupadd: cannot lock /etc/group".into(),
            })
        );
        assert!(classify("useradd", "x", Some(0), b"").is_ok());
    }

    #[test]
    fn nothing_else_is_an_account_command() {
        assert_eq!(command(&Action::DaemonReload), None);
        assert_eq!(command(&Action::Commit), None);
    }

    #[test]
    fn a_group_and_a_user_this_host_has_are_observed() {
        assert_eq!(
            observe(&Query::Group("root".into())),
            Some(Fact::Group(Some(GroupFacts {
                gid: 0,
                members: group("root").unwrap().members,
            })))
        );
        assert!(matches!(
            observe(&Query::User("root".into())),
            Some(Fact::User(Some(UserFacts { uid: 0, gid: 0, .. })))
        ));
        assert_eq!(
            observe(&Query::User("mix-no-such-user".into())),
            Some(Fact::User(None))
        );
    }
}
