use std::sync::Arc;

use futures_util::StreamExt;
use mix_events::v1::{
    Command, Envelope, NodeProgress, OutputLine, RepairRequest, Stream, command, envelope,
    node_progress,
};
use mix_rpc::{Caller, Client, Controls, Events, Reply, Worker, serve_connection};

fn main() {
    divan::main();
}

const VERSION: &str = "1.2.3";

struct Replay(Arc<[Envelope]>);

impl Worker for Replay {
    const VERSION: &'static str = VERSION;

    fn admits(&self, _caller: Caller) -> bool {
        true
    }

    async fn run(&self, _caller: Caller, _command: Command, _controls: Controls, events: Events) {
        for envelope in self.0.iter() {
            let _ = events.send(Reply::Envelope(envelope.clone()));
        }
    }
}

fn lines(count: usize) -> Vec<Envelope> {
    (0..count)
        .map(|line| Envelope {
            seq: line as u64 + 1,
            request: "0f6c2b9e-6a43-4f0e-9a5c-3c1e8d2a7b51".to_string(),
            event: Some(envelope::Event::NodeProgress(NodeProgress {
                id: 7,
                progress: Some(node_progress::Progress::Line(OutputLine {
                    text: format!(
                        "copying path '/nix/store/{line:032}-package-{line}' from 'https://cache.nixos.org'..."
                    ),
                    stream: Stream::Stderr as i32,
                })),
            })),
        })
        .collect()
}

fn repair() -> Command {
    Command {
        request: Some(command::Request::Repair(RepairRequest {})),
        ..Command::default()
    }
}

#[divan::bench(args = [16, 256, 2048])]
fn stream_a_request(bencher: divan::Bencher, count: usize) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let envelopes: Arc<[Envelope]> = lines(count).into();
    let command = repair();

    bencher.bench_local(|| {
        runtime.block_on(async {
            let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
            let server = tokio::spawn(serve_connection(Replay(Arc::clone(&envelopes)), theirs));
            let mut client = Client::connect(ours, VERSION).await.unwrap();
            let (controls, replies) = client.run(&command).await.unwrap();
            let received = replies
                .fold(0usize, |seen, _| async move { seen + 1 })
                .await;
            drop(controls);
            drop(client);
            server.await.unwrap().unwrap();
            received
        })
    });
}
