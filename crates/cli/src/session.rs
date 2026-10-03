use std::path::Path;
use std::process::ExitCode;

use mix_events::v1::command::Request;
use mix_render::View;

use crate::client::{self, Failure};
use crate::request;

pub async fn run(request: Request, view: &View, socket: &Path) -> ExitCode {
    let result = match request::refused(&request) {
        Some(fault) => Err(Failure::Failed(Box::new(fault))),
        None => client::run(&request, request::route(&request), view, socket).await,
    };
    if let Err(failure) = &result {
        view.failed(
            uuid::Uuid::now_v7().to_string(),
            &request,
            &failure.fault(),
            failure.source(),
        );
    }
    let from_root = view
        .exit
        .code()
        .map(|code| ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX)));
    match result {
        Ok(()) => from_root.unwrap_or(ExitCode::SUCCESS),
        Err(_) => from_root.unwrap_or(ExitCode::FAILURE),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;
    use std::sync::Arc;

    use mix_events::v1::command::Request;
    use mix_events::v1::{
        Code, Command, Envelope, ExplainRequest, InstallRequest, NodeFinished, envelope,
        node_finished,
    };
    use mix_events::{Detail, Ending, Outbox, ROOT, Render, Start, Tree};
    use mix_render::{Exit, Format, View};
    use mix_rpc::{Caller, Controls, Events, Reply, Worker};

    use super::run;

    struct Forward(Events);

    impl Render for Forward {
        fn envelope(&mut self, envelope: Envelope) {
            let _ = self.0.send(Reply::Envelope(envelope));
        }

        fn detail(&self) -> Detail {
            Detail::Trace
        }
    }

    struct RequestHost;

    impl Worker for RequestHost {
        const VERSION: &'static str = env!("CARGO_PKG_VERSION");

        fn admits(&self, _caller: Caller) -> bool {
            true
        }

        async fn run(
            &self,
            _caller: Caller,
            command: Command,
            _controls: Controls,
            events: Events,
        ) {
            let session =
                mix_shell::Session::new(mix_exec::Scope::root()).with_render(Forward(events));
            mix_shell::request::run(&session, command).await;
        }
    }

    struct EndsWithProblems;

    impl Worker for EndsWithProblems {
        const VERSION: &'static str = env!("CARGO_PKG_VERSION");

        fn admits(&self, _caller: Caller) -> bool {
            true
        }

        async fn run(
            &self,
            _caller: Caller,
            command: Command,
            _controls: Controls,
            events: Events,
        ) {
            let outbox = Arc::new(Outbox::new("request", || {}));
            let key = mix_events::key_of(command.request.as_ref());
            let mut tree = Tree::new(
                Arc::clone(&outbox),
                Arc::new(|| None),
                Start::command(key, command),
            );
            tree.finish(ROOT, Ending::succeeded().for_root(true))
                .unwrap();
            drop(tree);
            for envelope in outbox.drain() {
                let _ = events.send(Reply::Envelope(envelope));
            }
        }
    }

    struct EndsSilently;

    impl Worker for EndsSilently {
        const VERSION: &'static str = env!("CARGO_PKG_VERSION");

        fn admits(&self, _caller: Caller) -> bool {
            true
        }

        async fn run(
            &self,
            _caller: Caller,
            _command: Command,
            _controls: Controls,
            _events: Events,
        ) {
        }
    }

    fn listening<W: Worker>(directory: &Path, worker: fn() -> W) -> PathBuf {
        let path = directory.join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(mix_rpc::serve_connection(worker(), stream));
            }
        });
        path
    }

    fn recording(directory: &Path) -> View {
        View {
            format: Format::Human,
            events_file: Some(directory.join("events.ndjson")),
            verbose: 0,
            quiet: true,
            exit: Exit::default(),
        }
    }

    fn root(directory: &Path) -> NodeFinished {
        let file = std::fs::File::open(directory.join("events.ndjson")).unwrap();
        let captured = mix_events::capture::read(std::io::BufReader::new(file)).unwrap();
        mix_events::validate(captured.envelopes.iter()).unwrap();
        captured
            .envelopes
            .into_iter()
            .find_map(|envelope| match envelope.event {
                Some(envelope::Event::NodeFinished(finished)) if finished.id == ROOT => {
                    Some(finished)
                }
                _ => None,
            })
            .expect("every run records its root")
    }

    fn explain(code: Code) -> Request {
        Request::Explain(ExplainRequest {
            code: Some(code as i32),
        })
    }

    #[tokio::test]
    async fn a_code_is_explained_by_the_daemon_like_every_other_command() {
        let directory = tempfile::tempdir().unwrap();
        let socket = listening(directory.path(), || RequestHost);

        let exit = run(
            explain(Code::Network),
            &recording(directory.path()),
            &socket,
        )
        .await;

        assert_eq!(exit, ExitCode::SUCCESS);
        assert_eq!(
            root(directory.path()).result,
            Some(node_finished::Result::Explain(
                mix_events::v1::ExplainResult {
                    codes: vec![Code::Network as i32],
                }
            ))
        );
    }

    #[tokio::test]
    async fn without_a_daemon_the_command_fails_as_not_set_up() {
        let directory = tempfile::tempdir().unwrap();

        let exit = run(
            explain(Code::Network),
            &recording(directory.path()),
            &directory.path().join("daemon.sock"),
        )
        .await;

        assert_eq!(exit, ExitCode::FAILURE);
        let root = root(directory.path());
        assert_eq!(root.diagnostic.unwrap().code(), Code::NotBootstrapped);
        assert_eq!(root.exit_code, mix_events::exit::FAILED);
    }

    #[tokio::test]
    async fn a_socket_nobody_listens_on_is_not_set_up_either() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());

        let exit = run(
            explain(Code::Network),
            &recording(directory.path()),
            &socket,
        )
        .await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(
            root(directory.path()).diagnostic.unwrap().code(),
            Code::NotBootstrapped
        );
    }

    #[tokio::test]
    async fn the_exit_code_is_the_one_the_daemons_root_ends_with() {
        let directory = tempfile::tempdir().unwrap();
        let socket = listening(directory.path(), || EndsWithProblems);
        let request = Request::Install(InstallRequest {
            packages: vec!["hello".to_string()],
        });

        let exit = run(request, &recording(directory.path()), &socket).await;

        assert_eq!(exit, ExitCode::from(3));
        assert_eq!(
            root(directory.path()).exit_code,
            mix_events::exit::PROBLEMS_REMAIN
        );
    }

    #[tokio::test]
    async fn a_worker_that_stops_without_an_ending_is_reported_as_ended() {
        let directory = tempfile::tempdir().unwrap();
        let socket = listening(directory.path(), || EndsSilently);

        let exit = run(
            explain(Code::Network),
            &recording(directory.path()),
            &socket,
        )
        .await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(
            root(directory.path()).diagnostic.unwrap().code(),
            Code::WorkerEnded
        );
    }
}
