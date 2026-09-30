use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use mix_rpc::{
    BootstrapRequest, Caller, Client, Event, Events, Failure, Mirror, Outcome, RepairReport,
    RepairRequest, TargetFailure, Unfixable, Worker, serve_connection,
};

struct Scripted;

impl Worker for Scripted {
    async fn bootstrap(
        &self,
        caller: Caller,
        request: BootstrapRequest,
        events: Events,
    ) -> Outcome {
        let _ = events.send(Event::Envelope(vec![8, 1]));
        let _ = events.send(Event::Envelope(
            format!(
                "caller {} mirror {:?} force {}",
                caller.uid,
                request.mirror.map(|mirror| mirror.url),
                request.force
            )
            .into_bytes(),
        ));
        if request.force {
            Outcome::Failure(Failure::Rollback {
                cause: Box::new(Failure::Interrupted),
                summary: "1 rollback step(s) failed".into(),
            })
        } else {
            Outcome::BootstrapDone
        }
    }

    async fn repair(&self, _caller: Caller, _request: RepairRequest, _events: Events) -> Outcome {
        Outcome::RepairDone {
            interrupted: false,
            reports: vec![
                RepairReport {
                    name: "/nix".into(),
                    failure: Some(TargetFailure::Unrepairable {
                        artifact: "/nix".into(),
                        reason: Unfixable::NotADirectory,
                    }),
                },
                RepairReport {
                    name: "/etc/nix/nix.conf".into(),
                    failure: None,
                },
            ],
        }
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

fn request(force: bool) -> BootstrapRequest {
    BootstrapRequest {
        mirror: Some(Mirror {
            url: "http://mirror.internal".into(),
            key: None,
        }),
        force,
    }
}

#[tokio::test]
async fn a_request_streams_its_events_in_order_and_ends_with_one_outcome() {
    let (mut client, _server) = connected().await;

    let events: Vec<Event> = client
        .bootstrap(&request(false))
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;

    assert_eq!(events.len(), 3, "{events:?}");
    assert!(matches!(&events[0], Event::Envelope(bytes) if bytes == &[8, 1]));
    assert!(matches!(&events[1], Event::Envelope(bytes) if bytes.starts_with(b"caller ")));
    assert!(matches!(
        &events[2],
        Event::Finished(Outcome::BootstrapDone)
    ));
}

#[tokio::test]
async fn the_worker_learns_who_called_from_the_kernel_not_the_request() {
    let (mut client, _server) = connected().await;

    let events: Vec<Event> = client
        .bootstrap(&request(false))
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;

    let Event::Envelope(bytes) = &events[1] else {
        panic!("expected an envelope, got {:?}", events[1]);
    };
    assert_eq!(
        String::from_utf8_lossy(bytes),
        format!(
            "caller {} mirror Some(\"http://mirror.internal\") force false",
            current_uid()
        )
    );
}

#[tokio::test]
async fn a_typed_failure_arrives_intact() {
    let (mut client, _server) = connected().await;

    let last = client
        .bootstrap(&request(true))
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
        .await
        .pop()
        .unwrap();

    assert_eq!(
        format!("{last:?}"),
        format!(
            "{:?}",
            Event::Finished(Outcome::Failure(Failure::Rollback {
                cause: Box::new(Failure::Interrupted),
                summary: "1 rollback step(s) failed".into(),
            }))
        )
    );
}

#[tokio::test]
async fn repair_reports_every_item_it_looked_at() {
    let (mut client, _server) = connected().await;

    let events: Vec<Event> = client
        .repair(&RepairRequest)
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;

    let [Event::Finished(Outcome::RepairDone { reports, .. })] = events.as_slice() else {
        panic!("expected only the outcome, got {events:?}");
    };
    assert_eq!(reports.len(), 2);
    assert!(reports[0].failure.is_some());
    assert!(reports[1].failure.is_none());
}

#[tokio::test]
async fn the_worker_stops_once_its_client_is_gone() {
    let (mut client, server) = connected().await;
    let _ = client
        .bootstrap(&request(false))
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

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
    async fn bootstrap(
        &self,
        _caller: Caller,
        _request: BootstrapRequest,
        events: Events,
    ) -> Outcome {
        let _ = events.send(Event::Envelope(Vec::new()));
        events.closed().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.cleaned_up.store(true, Ordering::SeqCst);
        Outcome::Failure(Failure::Interrupted)
    }

    async fn repair(&self, _caller: Caller, _request: RepairRequest, _events: Events) -> Outcome {
        Outcome::RepairDone {
            reports: Vec::new(),
            interrupted: false,
        }
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
    let mut events = client.bootstrap(&request(false)).await.unwrap();
    assert!(matches!(
        events.next().await,
        Some(Ok(Event::Envelope(bytes))) if bytes.is_empty()
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
    async fn bootstrap(
        &self,
        _caller: Caller,
        _request: BootstrapRequest,
        events: Events,
    ) -> Outcome {
        let _ = events.send(Event::Envelope(Vec::new()));
        self.release.notified().await;
        Outcome::BootstrapDone
    }

    async fn repair(&self, _caller: Caller, _request: RepairRequest, _events: Events) -> Outcome {
        Outcome::RepairDone {
            reports: Vec::new(),
            interrupted: false,
        }
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
    let mut first = client.bootstrap(&request(false)).await.unwrap();
    assert!(matches!(
        first.next().await,
        Some(Ok(Event::Envelope(bytes))) if bytes.is_empty()
    ));

    let second = client.repair(&RepairRequest).await;

    let Err(mix_rpc::Error::Refused(reason)) = second else {
        panic!("a second request must be refused while the first is running");
    };
    assert!(reason.contains("already"), "{reason}");
    release.notify_one();
    assert!(matches!(
        first.next().await,
        Some(Ok(Event::Finished(Outcome::BootstrapDone)))
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
    let mut events = client.bootstrap(&request(false)).await.unwrap();
    assert!(matches!(events.next().await, Some(Ok(Event::Envelope(_)))));

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
