#![allow(clippy::disallowed_methods)]

use std::io::Cursor;
use std::sync::Arc;

use mix_core::declared::identity::InvokingUser;
use mix_core::effect::{Expect, Fact, Failure, PathFacts, Performed, Query};

use super::*;

fn lines(requests: &[Request]) -> Cursor<Vec<u8>> {
    let mut input = Vec::new();
    for request in requests {
        serde_json::to_writer(&mut input, request).unwrap();
        input.push(b'\n');
    }
    Cursor::new(input)
}

fn replies(output: &[u8]) -> Vec<Reply> {
    output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn put(path: &Path, expect: Expect) -> Action {
    Action::PutFile {
        path: path.to_path_buf(),
        contents: Arc::from(&b"managed"[..]),
        mode: 0o644,
        owner: None,
        expect,
    }
}

#[test]
fn a_change_is_announced_and_made_only_after_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    let mut output = Vec::new();

    serve(
        "r1",
        lines(&[
            Request::Perform(put(&path, Expect::Absent)),
            Request::Proceed,
        ]),
        &mut output,
    )
    .unwrap();

    let replies = replies(&output);
    assert!(matches!(&replies[0], Reply::Prepared(undo) if !undo.is_empty()));
    assert!(matches!(
        &replies[1],
        Reply::Done(Ok(Performed { undo })) if !undo.is_empty()
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"managed");
}

#[test]
fn a_refused_change_is_not_made() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    let mut output = Vec::new();

    serve(
        "r1",
        lines(&[
            Request::Perform(put(&path, Expect::Absent)),
            Request::Refuse(Failure::Cancelled),
        ]),
        &mut output,
    )
    .unwrap();

    assert!(matches!(
        replies(&output).last(),
        Some(Reply::Done(Err(Failure::Cancelled)))
    ));
    assert!(!path.exists());
}

#[test]
fn only_file_actions_are_carried_out() {
    let mut output = Vec::new();

    serve(
        "r1",
        lines(&[Request::Perform(Action::DaemonReload)]),
        &mut output,
    )
    .unwrap();

    assert!(matches!(
        replies(&output)[..],
        [Reply::Done(Err(Failure::CommandFailed { .. }))]
    ));
}

#[test]
fn an_answer_without_a_question_ends_the_session() {
    let mut output = Vec::new();

    let served = serve("r1", lines(&[Request::Proceed]), &mut output);

    assert!(served.is_err());
    assert!(output.is_empty());
}

#[test]
fn a_set_aside_file_is_removed_on_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    std::fs::write(&path, "old").unwrap();
    let mut output = Vec::new();
    let files = Files::open(Path::new("/"), "observe").unwrap();
    let Some(Fact::Path(PathFacts { id: Some(id), .. })) =
        files.observe(&Query::Path(path.clone()))
    else {
        panic!("the file exists");
    };

    serve(
        "r1",
        lines(&[
            Request::Perform(put(&path, Expect::Present(id))),
            Request::Proceed,
            Request::Perform(Action::Commit),
        ]),
        &mut output,
    )
    .unwrap();

    assert!(matches!(replies(&output).last(), Some(Reply::Done(Ok(_)))));
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, ["config"]);
}

#[test]
fn the_agent_answers_which_generation_was_built_from_the_current_files() {
    let home = tempfile::tempdir().unwrap();
    let mut output = Vec::new();

    serve(
        "r1",
        lines(&[Request::FindBuilt(me(home.path()))]),
        &mut output,
    )
    .unwrap();

    assert!(matches!(replies(&output).as_slice(), [Reply::Built(None)]));
}

fn me(home: &Path) -> InvokingUser {
    InvokingUser {
        uid: nix::unistd::Uid::current().as_raw(),
        gid: nix::unistd::Gid::current().as_raw(),
        name: "mix-user".into(),
        home: home.to_path_buf(),
    }
}

#[test]
fn the_agent_observes_paths_and_profiles_as_its_user() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    std::fs::write(&config, b"mine").unwrap();
    let mut output = Vec::new();

    serve(
        "r1",
        lines(&[
            Request::Observe(Query::Contents(config.clone())),
            Request::Observe(Query::Profile(me(home.path()))),
            Request::Exists(config),
            Request::Exists(home.path().join("missing")),
            Request::Active(me(home.path())),
        ]),
        &mut output,
    )
    .unwrap();

    let replies = replies(&output);
    assert!(
        matches!(&replies[0], Reply::Observed(Some(Fact::Contents(Some(bytes)))) if **bytes == *b"mine")
    );
    assert!(matches!(
        &replies[1],
        Reply::Observed(Some(Fact::Profile(profile))) if profile.generations.is_empty()
    ));
    assert!(matches!(replies[2], Reply::Exists(true)));
    assert!(matches!(replies[3], Reply::Exists(false)));
    assert!(matches!(replies[4], Reply::Active(None)));
}

#[test]
fn the_agent_refuses_a_question_about_another_user() {
    let home = tempfile::tempdir().unwrap();
    let other = InvokingUser {
        uid: nix::unistd::Uid::current().as_raw() + 1,
        ..me(home.path())
    };
    let mut output = Vec::new();

    let refused = serve("r1", lines(&[Request::Active(other)]), &mut output);

    assert!(refused.is_err());
    assert!(output.is_empty());
}

#[test]
fn a_tree_holding_another_owners_files_is_neither_set_aside_nor_removed() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(tree.join("inner")).unwrap();
    let files = Files::open(Path::new("/"), "r1").unwrap();
    let Some(Fact::Path(PathFacts { id: Some(id), .. })) =
        files.observe(&Query::Path(tree.clone()))
    else {
        panic!("the tree is there");
    };
    let me = nix::unistd::Uid::current().as_raw();

    for action in [
        Action::SetAside {
            path: tree.clone(),
            expect: id,
        },
        Action::RemoveCreatedTree {
            path: tree.clone(),
            expect: id,
        },
    ] {
        assert_eq!(refused(&files, &action, me), None);
        assert!(matches!(
            refused(&files, &action, me + 1),
            Some(Failure::Conflict { subject, .. }) if subject == tree.display().to_string()
        ));
    }
    assert_eq!(refused(&files, &put(&tree, Expect::Absent), me + 1), None);
}
