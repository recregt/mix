use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use mix_exec::Scope;

mix_testchild::install!();

fn child<'s>(steps: impl IntoIterator<Item = &'s str>) -> mix_exec::Command {
    mix_exec::Command::new(mix_testchild::program()).args(mix_testchild::args(steps))
}

struct Running {
    session: mix_exec::Session,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

async fn running(request: &Scope) -> Running {
    let (session, stdin, stdout) = child(["pid", "echo:got ", "sleep"])
        .session(request)
        .unwrap();
    let mut lines = BufReader::new(stdout).lines();
    lines
        .next_line()
        .await
        .unwrap()
        .expect("the child says it runs");
    Running {
        session,
        stdin,
        lines,
    }
}

#[tokio::test]
async fn kill_stops_every_running_child_of_the_request() {
    let request = Scope::root();
    let ordinary = running(&request.child()).await;
    let shielded = running(&request.shielded()).await;

    request.processes().kill();

    for child in [ordinary, shielded] {
        let output = child.session.finish().await.unwrap();
        assert!(!output.status.success());
    }
}

#[tokio::test]
async fn killing_one_request_leaves_another_running() {
    let killed = Scope::root();
    let other = Scope::root();
    let mut alive = running(&other).await;

    killed.processes().kill();
    killed.processes().pause();

    alive.stdin.write_all(b"still here\n").await.unwrap();
    assert_eq!(
        alive.lines.next_line().await.unwrap().as_deref(),
        Some("got still here"),
        "the other request's child neither died nor froze"
    );
    other.processes().kill();
    alive.session.finish().await.unwrap();
}

#[tokio::test]
async fn a_session_talks_both_ways_and_reports_how_it_ended() {
    let request = Scope::root();
    let (session, mut stdin, stdout) = child(["echo:got ", "eprint:done", "exit:3"])
        .session(&request)
        .unwrap();

    stdin.write_all(b"hello\n").await.unwrap();
    let reply = BufReader::new(stdout).lines().next_line().await.unwrap();
    drop(stdin);
    let output = session.finish().await.unwrap();

    assert_eq!(reply.as_deref(), Some("got hello"));
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stderr, b"done\n");
}

#[derive(Default)]
struct Started(std::sync::Mutex<Vec<String>>);

impl mix_exec::Watch for Started {
    fn started(&self, command: &str) {
        self.0.lock().unwrap().push(command.to_string());
    }

    fn finished(&self, command: &str, status: std::process::ExitStatus) {
        self.0
            .lock()
            .unwrap()
            .push(format!("{command} exited {:?}", status.code()));
    }
}

#[tokio::test]
async fn a_watched_scope_hears_of_every_command_it_starts_and_how_it_ended() {
    let started = std::sync::Arc::new(Started::default());
    let request = Scope::root().watched(started.clone());

    child([]).output(&request.child()).await.unwrap();
    child(["print:two words"])
        .output(&request.shielded())
        .await
        .unwrap();
    child(["exit:1"]).output(&request).await.unwrap();
    child([]).output(&Scope::root()).await.unwrap();

    let program = mix_testchild::program().display().to_string();
    assert_eq!(
        *started.0.lock().unwrap(),
        [
            format!("{program} --mix-test-child"),
            format!("{program} --mix-test-child exited Some(0)"),
            format!(r#"{program} --mix-test-child "print:two words""#),
            format!(r#"{program} --mix-test-child "print:two words" exited Some(0)"#),
            format!("{program} --mix-test-child exit:1"),
            format!("{program} --mix-test-child exit:1 exited Some(1)"),
        ]
    );
}
