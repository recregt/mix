use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use mix_events::v1::{
    BootstrapRequest, Command, Envelope, NodeStarted, RepairRequest, command, envelope,
    node_started,
};
use mix_rpc::{Caller, Client, Control, Controls, Events, Reply, Worker, serve_connection};

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

const VERSION: &str = "1.2.3";

struct Scripted;

impl Worker for Scripted {
    const VERSION: &'static str = VERSION;

    fn admits(&self, _caller: Caller) -> bool {
        true
    }

    async fn run(&self, caller: Caller, command: Command, _controls: Controls, events: Events) {
        let _ = events.send(Reply::Envelope(marked(1, String::new())));
        let _ = events.send(Reply::Envelope(marked(2, format!("caller {}", caller.uid))));
        let _ = events.send(Reply::Envelope(echo(command)));
    }
}

fn current_uid() -> u32 {
    std::fs::metadata("/proc/self").unwrap().uid()
}

async fn connected() -> (Client, tokio::task::JoinHandle<Result<(), mix_rpc::Error>>) {
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let server = tokio::spawn(serve_connection(Scripted, theirs));
    (Client::connect(ours, VERSION).await.unwrap(), server)
}

fn bootstrap(force: bool) -> Command {
    Command {
        mix_version: "1.2.3".into(),
        schema_minor: mix_events::SCHEMA_MINOR,
        request: Some(command::Request::Bootstrap(Box::new(BootstrapRequest {
            force,
            mirror: Some("http://mirror.internal".into()),
            mirror_key: Some("mirror:KEY".into()),
        }))),
    }
}

fn repair() -> Command {
    Command {
        request: Some(command::Request::Repair(RepairRequest {})),
        ..Command::default()
    }
}

async fn every_envelope(client: &mut Client, command: &Command) -> Vec<Envelope> {
    let (_controls, replies) = client.run(command).await.unwrap();
    replies
        .map(|reply| match reply.unwrap() {
            Reply::Envelope(envelope) => envelope,
            Reply::Applied(control) => panic!("nothing was asked of the worker, yet {control:?}"),
        })
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
    const VERSION: &'static str = VERSION;

    fn admits(&self, _caller: Caller) -> bool {
        true
    }

    async fn run(&self, _caller: Caller, _command: Command, _controls: Controls, events: Events) {
        let _ = events.send(Reply::Envelope(marked(1, String::new())));
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
    let mut client = Client::connect(ours, VERSION).await.unwrap();
    let (controls, mut events) = client.run(&bootstrap(false)).await.unwrap();
    assert!(matches!(
        events.next().await,
        Some(Ok(Reply::Envelope(envelope))) if envelope.seq == 1
    ));

    drop(controls);
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
    const VERSION: &'static str = VERSION;

    fn admits(&self, _caller: Caller) -> bool {
        true
    }

    async fn run(&self, _caller: Caller, _command: Command, _controls: Controls, events: Events) {
        let _ = events.send(Reply::Envelope(marked(1, String::new())));
        self.release.notified().await;
        let _ = events.send(Reply::Envelope(marked(2, String::new())));
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
    let mut client = Client::connect(ours, VERSION).await.unwrap();
    let (_controls, mut first) = client.run(&bootstrap(false)).await.unwrap();
    assert!(matches!(
        first.next().await,
        Some(Ok(Reply::Envelope(envelope))) if envelope.seq == 1
    ));

    let second = client.run(&repair()).await.map(drop);

    let Err(mix_rpc::Error::Refused(reason)) = second else {
        panic!("a second request must be refused while the first is running");
    };
    assert!(reason.contains("already"), "{reason}");
    release.notify_one();
    assert!(matches!(
        first.next().await,
        Some(Ok(Reply::Envelope(envelope))) if envelope.seq == 2
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
    let mut client = Client::connect(tokio::net::UnixStream::from_std(ours).unwrap(), VERSION)
        .await
        .unwrap();
    let (_controls, mut events) = client.run(&bootstrap(false)).await.unwrap();
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
async fn a_program_that_exits_without_answering_is_refused() {
    let started = tokio::time::timeout(
        Duration::from_secs(5),
        Client::start(std::path::Path::new("sh"), &["-c", "exit 3"], None, VERSION),
    )
    .await
    .expect("a program that exited cannot keep the client waiting");

    assert!(
        matches!(
            started,
            Err(mix_rpc::Error::Refused(_) | mix_rpc::Error::Connect(_))
        ),
        "the program may exit before or after the connection is up, and either way it was \
         never a worker: {:?}",
        started.err()
    );
}

#[tokio::test]
async fn a_worker_that_cannot_start_is_named_in_the_error() {
    let missing = Client::start(
        std::path::Path::new("mix-no-such-program"),
        &[],
        None,
        VERSION,
    )
    .await
    .err()
    .expect("a missing program cannot start");

    assert!(
        std::error::Error::source(&missing)
            .is_some_and(|source| source.to_string().contains("mix-no-such-program")),
        "{missing}"
    );
}

struct Obeys;

impl Worker for Obeys {
    const VERSION: &'static str = VERSION;

    fn admits(&self, _caller: Caller) -> bool {
        true
    }

    async fn run(
        &self,
        _caller: Caller,
        _command: Command,
        mut controls: Controls,
        events: Events,
    ) {
        let _ = events.send(Reply::Envelope(marked(1, String::new())));
        while let Some(control) = controls.recv().await {
            let _ = events.send(Reply::Applied(control));
            if control == Control::Interrupt {
                return;
            }
        }
    }
}

#[tokio::test]
async fn each_control_reaches_the_worker_in_order_and_comes_back_applied() {
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let _server = tokio::spawn(serve_connection(Obeys, theirs));
    let mut client = Client::connect(ours, VERSION).await.unwrap();
    let (controls, replies) = client.run(&bootstrap(false)).await.unwrap();
    let mut replies = std::pin::pin!(replies);
    assert!(matches!(replies.next().await, Some(Ok(Reply::Envelope(_)))));

    for control in [Control::Pause, Control::Resume, Control::Interrupt] {
        controls.send(control);
        assert_eq!(
            replies.next().await.unwrap().unwrap(),
            Reply::Applied(control)
        );
    }
    assert!(replies.next().await.is_none());
}

#[tokio::test]
async fn a_worker_of_another_version_is_refused_before_any_request() {
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let _server = tokio::spawn(serve_connection(Scripted, theirs));

    let refused = Client::connect(ours, "9.9.9").await.err();

    let Some(mix_rpc::Error::VersionMismatch { ours, theirs }) = refused else {
        panic!("a worker of another version must be refused, got {refused:?}");
    };
    assert_eq!((ours.as_str(), theirs.as_str()), ("9.9.9", VERSION));
}

struct Refuses;

impl Worker for Refuses {
    const VERSION: &'static str = VERSION;

    fn admits(&self, _caller: Caller) -> bool {
        false
    }

    async fn run(&self, _caller: Caller, _command: Command, _controls: Controls, _events: Events) {
        panic!("a refused caller must never reach the worker");
    }
}

#[tokio::test]
async fn a_caller_the_worker_does_not_admit_is_denied_before_any_request() {
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let _server = tokio::spawn(serve_connection(Refuses, theirs));

    let denied = Client::connect(ours, VERSION).await.err();

    assert!(
        matches!(denied, Some(mix_rpc::Error::Denied(_))),
        "{denied:?}"
    );
}

struct CountsAdmissions(Arc<std::sync::atomic::AtomicUsize>);

impl Worker for CountsAdmissions {
    const VERSION: &'static str = VERSION;

    fn admits(&self, _caller: Caller) -> bool {
        self.0.fetch_add(1, Ordering::SeqCst);
        true
    }

    async fn run(&self, _caller: Caller, _command: Command, _controls: Controls, _events: Events) {}
}

#[tokio::test]
async fn a_connection_checks_its_caller_once() {
    let admissions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let _server = tokio::spawn(serve_connection(
        CountsAdmissions(Arc::clone(&admissions)),
        theirs,
    ));
    let mut client = Client::connect(ours, VERSION).await.unwrap();

    every_envelope(&mut client, &repair()).await;

    assert_eq!(admissions.load(Ordering::SeqCst), 1);
}
