use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use mix_rpc::{
    BootstrapRequest, Caller, Client, Event, Events, Failure, Level, Mirror, Outcome, RepairReport,
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
        let _ = events.send(Event::SpanOpened {
            id: 1,
            parent: None,
            name: "step".into(),
            fields: vec![("name".into(), "create nix dir".into())],
        });
        let _ = events.send(Event::Log {
            level: Level::Info,
            span: Some(1),
            message: format!(
                "caller {} mirror {:?} force {}",
                caller.uid,
                request.mirror.map(|mirror| mirror.url),
                request.force
            ),
        });
        let _ = events.send(Event::SpanClosed {
            id: 1,
            failed: false,
        });
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
        Outcome::RepairDone(vec![
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
        ])
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
        log_level: Level::Info,
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

    assert_eq!(events.len(), 4, "{events:?}");
    assert!(matches!(&events[0], Event::SpanOpened { id: 1, name, .. } if name == "step"));
    assert!(matches!(
        &events[2],
        Event::SpanClosed {
            id: 1,
            failed: false
        }
    ));
    assert!(matches!(
        &events[3],
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

    let Event::Log { message, .. } = &events[1] else {
        panic!("expected a log, got {:?}", events[1]);
    };
    assert_eq!(
        message,
        &format!(
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
        .repair(&RepairRequest {
            log_level: Level::Warn,
        })
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;

    let [Event::Finished(Outcome::RepairDone(reports))] = events.as_slice() else {
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
        let _ = events.send(Event::ActivityLine("started".into()));
        events.closed().await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.cleaned_up.store(true, Ordering::SeqCst);
        Outcome::Failure(Failure::Interrupted)
    }

    async fn repair(&self, _caller: Caller, _request: RepairRequest, _events: Events) -> Outcome {
        Outcome::RepairDone(Vec::new())
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
        Some(Ok(Event::ActivityLine(line))) if line == "started"
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
