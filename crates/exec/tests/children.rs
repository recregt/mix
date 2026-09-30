use std::time::Duration;

use mix_exec::Scope;

fn sleep_in(
    request: &Scope,
) -> tokio::task::JoinHandle<Result<std::process::Output, mix_exec::Error>> {
    let request = request.clone();
    tokio::spawn(async move {
        mix_exec::Command::new("sleep")
            .arg("30")
            .output(&request)
            .await
    })
}

#[tokio::test]
async fn kill_stops_every_running_child_of_the_request() {
    let request = mix_exec::Scope::root();
    let running = sleep_in(&request.child());
    let shielded = sleep_in(&request.shielded());
    tokio::time::sleep(Duration::from_millis(200)).await;

    request.processes().kill();

    for child in [running, shielded] {
        let output = tokio::time::timeout(Duration::from_secs(5), child)
            .await
            .expect("a killed child ends at once")
            .unwrap()
            .unwrap();
        assert!(!output.status.success());
    }
}

#[tokio::test]
async fn killing_one_request_leaves_another_running() {
    let killed = mix_exec::Scope::root();
    let other = mix_exec::Scope::root();
    let running = sleep_in(&other);
    tokio::time::sleep(Duration::from_millis(200)).await;

    killed.processes().kill();
    killed.processes().pause();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(!running.is_finished());
    running.abort();
}

#[tokio::test]
async fn a_session_talks_both_ways_and_reports_how_it_ended() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let request = mix_exec::Scope::root();
    let (session, mut stdin, stdout) = mix_exec::Command::new("sh")
        .args(["-c", "read line; echo \"got $line\"; echo done >&2; exit 3"])
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
    let request = mix_exec::Scope::root().watched(started.clone());

    mix_exec::Command::new("true")
        .output(&request.child())
        .await
        .unwrap();
    mix_exec::Command::new("echo")
        .arg("two words")
        .output(&request.shielded())
        .await
        .unwrap();
    mix_exec::Command::new("false")
        .output(&request)
        .await
        .unwrap();
    mix_exec::Command::new("true")
        .output(&mix_exec::Scope::root())
        .await
        .unwrap();

    assert_eq!(
        *started.0.lock().unwrap(),
        [
            "true",
            "true exited Some(0)",
            r#"echo "two words""#,
            r#"echo "two words" exited Some(0)"#,
            "false",
            "false exited Some(1)"
        ]
    );
}
