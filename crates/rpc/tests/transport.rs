use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use mix_events::v1::{
    BootstrapRequest, Command, Envelope, NodeStarted, RepairRequest, command, envelope,
    node_started,
};
use mix_rpc::{Caller, Client, Events, Worker, serve_connection};

fn marked(seq: u64, request: String) -> Envelope {
    Envelope {
        seq,
        request,
        event: None,
    }
}

fn echo(command: Command) -> Envelope {
    Envelope {
        seq: 3,
        request: String::new(),
        event: Some(envelope::Event::NodeStarted(NodeStarted {
            kind: Some(node_started::Kind::Command(command)),
            ..NodeStarted::default()
        })),
    }
}

struct Scripted;

impl Worker for Scripted {
    async fn run(&self, caller: Caller, command: Command, events: Events) {
        let _ = events.send(marked(1, String::new()));
        let _ = events.send(marked(2, format!("caller {}", caller.uid)));
        let _ = events.send(echo(command));
    }
}

fn current_uid() -> u32 {
    std::fs::metadata("/proc/self").unwrap().uid()
}

async fn connected() -> (Client, tokio::task::JoinHandle<Result<(), mix_rpc::Error>>) {
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let server = tokio::spawn(serve_connection(Scripted, theirs));
    (Client::connect(ours).await.unwrap(), server)
}

fn bootstrap(force: bool) -> Command {
    Command {
        mix_version: "1.2.3".into(),
        schema_minor: mix_events::SCHEMA_MINOR,
        request: Some(command::Request::Bootstrap(BootstrapRequest {
            force,
            mirror: Some("http://mirror.internal".into()),
            mirror_key: Some("mirror:KEY".into()),
        })),
    }
}

fn repair() -> Command {
    Command {
        request: Some(command::Request::Repair(RepairRequest {})),
        ..Command::default()
    }
}

async fn every_envelope(client: &mut Client, command: &Command) -> Vec<Envelope> {
    client
        .run(command)
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await
}

#[tokio::test]
async fn a_request_streams_its_envelopes_in_order() {
    let (mut client, _server) = connected().await;

    let envelopes = every_envelope(&mut client, &bootstrap(false)).await;

    assert_eq!(
        envelopes
            .iter()
            .map(|envelope| envelope.seq)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[tokio::test]
async fn the_worker_learns_who_called_from_the_kernel_not_the_request() {
    let (mut client, _server) = connected().await;

    let envelopes = every_envelope(&mut client, &bootstrap(false)).await;

    assert_eq!(envelopes[1].request, format!("caller {}", current_uid()));
}

#[tokio::test]
async fn the_command_reaches_the_worker_as_it_was_sent() {
    let (mut client, _server) = connected().await;

    let envelopes = every_envelope(&mut client, &bootstrap(true)).await;

    assert_eq!(envelopes[2], echo(bootstrap(true)));
}

#[tokio::test]
async fn the_worker_stops_once_its_client_is_gone() {
    let (mut client, server) = connected().await;
    let _ = every_envelope(&mut client, &bootstrap(false)).await;

    drop(client);

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the worker must shut down once its only client disconnects")
        .unwrap()
        .unwrap();
}

struct CleansUpWhenAbandoned {
    cleaned_up: Arc<AtomicBool>,
}

impl Worker for CleansUpWhenAbandoned {
    async fn run(&self, _caller: Caller, _command: Command, events: Events) {
        let _ = events.send(marked(1, String::new()));
        events.closed().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.cleaned_up.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn the_worker_waits_for_a_request_its_client_left() {
    let cleaned_up = Arc::new(AtomicBool::new(false));
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let server = tokio::spawn(serve_connection(
        CleansUpWhenAbandoned {
            cleaned_up: Arc::clone(&cleaned_up),
        },
        theirs,
    ));
    let mut client = Client::connect(ours).await.unwrap();
    let mut events = client.run(&bootstrap(false)).await.unwrap();
    assert!(matches!(
        events.next().await,
        Some(Ok(envelope)) if envelope.seq == 1
    ));

    drop(events);
    drop(client);

    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the worker must shut down once the request is over")
        .unwrap()
        .unwrap();
    assert!(
        cleaned_up.load(Ordering::SeqCst),
        "the worker returned before the abandoned request cleaned up"
    );
}

struct WaitsForRelease {
    release: Arc<tokio::sync::Notify>,
}

impl Worker for WaitsForRelease {
    async fn run(&self, _caller: Caller, _command: Command, events: Events) {
        let _ = events.send(marked(1, String::new()));
        self.release.notified().await;
        let _ = events.send(marked(2, String::new()));
    }
}

#[tokio::test]
async fn a_second_request_is_refused_while_the_first_is_running() {
    let release = Arc::new(tokio::sync::Notify::new());
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let _server = tokio::spawn(serve_connection(
        WaitsForRelease {
            release: Arc::clone(&release),
        },
        theirs,
    ));
    let mut client = Client::connect(ours).await.unwrap();
    let mut first = client.run(&bootstrap(false)).await.unwrap();
    assert!(matches!(
        first.next().await,
        Some(Ok(envelope)) if envelope.seq == 1
    ));

    let second = client.run(&repair()).await;

    let Err(mix_rpc::Error::Refused(reason)) = second else {
        panic!("a second request must be refused while the first is running");
    };
    assert!(reason.contains("already"), "{reason}");
    release.notify_one();
    assert!(matches!(
        first.next().await,
        Some(Ok(envelope)) if envelope.seq == 2
    ));
}

#[tokio::test]
async fn a_worker_that_dies_mid_request_reads_as_ended_not_refused() {
    let release = Arc::new(tokio::sync::Notify::new());
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    theirs.set_nonblocking(true).unwrap();
    ours.set_nonblocking(true).unwrap();
    let worker_process = tokio::runtime::Runtime::new().unwrap();
    worker_process.spawn(async move {
        serve_connection(
            WaitsForRelease { release },
            tokio::net::UnixStream::from_std(theirs).unwrap(),
        )
        .await
    });
    let mut client = Client::connect(tokio::net::UnixStream::from_std(ours).unwrap())
        .await
        .unwrap();
    let mut events = client.run(&bootstrap(false)).await.unwrap();
    assert!(matches!(events.next().await, Some(Ok(_))));

    worker_process.shutdown_background();

    let next = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("a dead worker ends the stream");
    assert!(
        matches!(next, None | Some(Err(mix_rpc::Error::Ended))),
        "{next:?}"
    );
}

#[tokio::test]
async fn waiting_for_the_worker_hands_back_how_it_exited() {
    let client = Client::start(
        std::path::Path::new("sh"),
        &["-c", "cat >/dev/null; exit 3"],
        None,
    )
    .await
    .unwrap();

    let status = tokio::time::timeout(Duration::from_secs(5), client.wait())
        .await
        .expect("waiting for an exited worker does not block")
        .unwrap()
        .unwrap();

    assert_eq!(status.code(), Some(3));
}

#[tokio::test]
async fn a_worker_that_cannot_start_is_named_in_the_error() {
    let missing = Client::start(std::path::Path::new("mix-no-such-program"), &[], None)
        .await
        .err()
        .expect("a missing program cannot start");

    assert!(
        std::error::Error::source(&missing)
            .is_some_and(|source| source.to_string().contains("mix-no-such-program")),
        "{missing}"
    );
}
