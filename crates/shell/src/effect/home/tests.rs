#![allow(clippy::disallowed_methods)]

use std::io::Cursor;
use std::sync::Arc;

use mix_core::action::{Expect, Performed};

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
        lines(&[Request::FindBuilt {
            home: home.path().to_path_buf(),
            generations: vec![1, 2],
        }]),
        &mut output,
    )
    .unwrap();

    assert!(matches!(replies(&output).as_slice(), [Reply::Built(None)]));
}
