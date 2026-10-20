use std::process::ExitCode;

use mix_events::v1::Command;
use mix_render::View;

use crate::client::{self, Failure};
use crate::request;

pub async fn run(
    command: Command,
    view: &View,
    daemon: &client::Daemon,
    unconfirmed: Option<mix_events::Fault>,
) -> ExitCode {
    let refused = unconfirmed.or_else(|| command.request.as_ref().and_then(request::refused));
    let result = match refused {
        Some(fault) => Err(Failure::Failed(Box::new(fault))),
        None => {
            let route = command
                .request
                .as_ref()
                .map_or(request::Route::Socket, request::route);
            client::run(&command, route, view, daemon).await
        }
    };
    if let Err(failure) = &result {
        view.failed(
            uuid::Uuid::now_v7().to_string(),
            &command,
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
        Err(failure) => from_root.unwrap_or_else(|| {
            let ending = mix_events::Ending::from(failure.fault()).for_root(false);
            ExitCode::from(u8::try_from(ending.exit_code()).unwrap_or(u8::MAX))
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::ExitCode;
    use std::sync::Arc;

    use mix_events::result::v1::{Result as Document, result};
    use mix_events::v1::command::Request;
    use mix_events::v1::{Code, Command, Envelope, ExplainRequest, InstallRequest};
    use mix_events::{Detail, Ending, Outbox, ROOT, Render, Start, Tree};
    use mix_render::{Exit, Json, View};
    use mix_rpc::{Caller, Controls, Events, Reply, Worker};

    use super::run;
    use crate::client;

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

    struct AnotherRelease;

    impl Worker for AnotherRelease {
        const VERSION: &'static str = "0.0.1-older";

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

    struct RefusesEveryone;

    impl Worker for RefusesEveryone {
        const VERSION: &'static str = env!("CARGO_PKG_VERSION");

        fn admits(&self, _caller: Caller) -> bool {
            false
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

    struct StopsMidway;

    impl Worker for StopsMidway {
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
            let tree = Tree::new(
                Arc::clone(&outbox),
                Arc::new(|| None),
                Start::command(key, command),
            );
            for envelope in outbox.drain() {
                let _ = events.send(Reply::Envelope(envelope));
            }
            std::mem::forget(tree);
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

    fn listening<W: Worker>(directory: &Path, worker: fn() -> W) -> client::Daemon {
        let daemon = client::Daemon::in_dir(directory);
        let listener = tokio::net::UnixListener::bind(&daemon.socket).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(mix_rpc::serve_connection(worker(), stream));
            }
        });
        daemon
    }

    #[derive(Default)]
    struct Recorded(Arc<std::sync::Mutex<Vec<Document>>>);

    impl Recorded {
        fn view(&self) -> View {
            View {
                json: Json::Into(Arc::clone(&self.0)),
                verbose: 0,
                quiet: true,
                exit: Exit::default(),
                departure: mix_render::Departure::default(),
                preview: false,
            }
        }

        fn document(&self) -> Document {
            let documents = self.0.lock().unwrap();
            let [document] = documents.as_slice() else {
                panic!("one document per run: {documents:?}");
            };
            document.clone()
        }
    }

    fn explain(code: Code) -> Command {
        mix_events::command(Request::Explain(ExplainRequest {
            code: Some(code as i32),
        }))
    }

    #[tokio::test]
    async fn a_code_is_explained_by_the_daemon_like_every_other_command() {
        let directory = tempfile::tempdir().unwrap();
        let socket = listening(directory.path(), || RequestHost);
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &socket, None).await;

        assert_eq!(exit, ExitCode::SUCCESS);
        assert_eq!(
            recorded.document().result,
            Some(result::Result::Explain(mix_events::v1::ExplainResult {
                codes: vec![Code::Network as i32],
            }))
        );
    }

    #[tokio::test]
    async fn without_a_daemon_the_command_fails_as_not_set_up() {
        let directory = tempfile::tempdir().unwrap();

        let recorded = Recorded::default();

        let exit = run(
            explain(Code::Network),
            &recorded.view(),
            &client::Daemon::in_dir(directory.path()),
            None,
        )
        .await;

        assert_eq!(exit, ExitCode::FAILURE);
        let document = recorded.document();
        assert_eq!(document.problems[0].code(), Code::NotBootstrapped);
        assert_eq!(document.exit, mix_events::exit::FAILED);
    }

    #[tokio::test]
    async fn a_socket_nobody_listens_on_is_not_set_up_either() {
        let directory = tempfile::tempdir().unwrap();
        let socket = client::Daemon::in_dir(directory.path());
        drop(std::os::unix::net::UnixListener::bind(&socket.socket).unwrap());
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &socket, None).await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(
            recorded.document().problems[0].code(),
            Code::NotBootstrapped
        );
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn on_a_machine_mix_set_up_a_daemon_that_is_not_listening_is_reported_as_such() {
        let directory = tempfile::tempdir().unwrap();
        let daemon = client::Daemon::in_dir(directory.path());
        std::fs::write(&daemon.unit, "[Socket]\n").unwrap();
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &daemon, None).await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(
            recorded.document().problems[0].code(),
            Code::DaemonUnavailable
        );
    }

    #[tokio::test]
    async fn a_daemon_of_another_release_is_named_as_such_and_not_retried() {
        let directory = tempfile::tempdir().unwrap();
        let daemon = listening(directory.path(), || AnotherRelease);
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &daemon, None).await;

        assert_eq!(exit, ExitCode::FAILURE);
        let problem = &recorded.document().problems[0];
        assert_eq!(problem.code(), Code::DaemonVersionMismatch);
        assert!(problem.detail.contains("0.0.1-older"), "{}", problem.detail);
        assert!(
            problem.detail.contains(env!("CARGO_PKG_VERSION")),
            "{}",
            problem.detail
        );
        assert!(
            problem
                .help
                .iter()
                .any(|help| help.contains("mix bootstrap")),
            "{:?}",
            problem.help
        );
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_daemon_that_refuses_the_caller_means_mix_is_not_set_up_for_them() {
        let directory = tempfile::tempdir().unwrap();
        let daemon = listening(directory.path(), || RefusesEveryone);
        std::fs::write(&daemon.unit, "[Socket]\n").unwrap();
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &daemon, None).await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(
            recorded.document().problems[0].code(),
            Code::NotBootstrapped
        );
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)]
    async fn a_daemon_that_closes_the_connection_unanswered_is_not_a_missing_right() {
        let directory = tempfile::tempdir().unwrap();
        let daemon = client::Daemon::in_dir(directory.path());
        std::fs::write(&daemon.unit, "[Socket]\n").unwrap();
        let listener = tokio::net::UnixListener::bind(&daemon.socket).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &daemon, None).await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(
            recorded.document().problems[0].code(),
            Code::DaemonUnavailable
        );
    }

    #[tokio::test]
    async fn the_exit_code_is_the_one_the_daemons_root_ends_with() {
        let directory = tempfile::tempdir().unwrap();
        let socket = listening(directory.path(), || EndsWithProblems);
        let command = mix_events::command(Request::Install(InstallRequest {
            packages: vec!["hello".to_string()],
        }));
        let recorded = Recorded::default();

        let exit = run(command, &recorded.view(), &socket, None).await;

        assert_eq!(exit, ExitCode::from(3));
        assert_eq!(recorded.document().exit, mix_events::exit::PROBLEMS_REMAIN);
    }

    #[tokio::test]
    async fn a_worker_that_stops_without_an_ending_is_reported_as_ended() {
        let directory = tempfile::tempdir().unwrap();
        let socket = listening(directory.path(), || EndsSilently);
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &socket, None).await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(recorded.document().problems[0].code(), Code::WorkerEnded);
    }

    #[tokio::test]
    async fn a_daemon_that_stops_partway_through_a_command_is_reported_as_ended() {
        let directory = tempfile::tempdir().unwrap();
        let socket = listening(directory.path(), || StopsMidway);
        let recorded = Recorded::default();

        let exit = run(explain(Code::Network), &recorded.view(), &socket, None).await;

        assert_eq!(exit, ExitCode::FAILURE);
        assert_eq!(recorded.document().problems[0].code(), Code::WorkerEnded);
    }

    #[tokio::test]
    async fn an_unconfirmed_removal_is_refused_before_any_daemon_is_asked() {
        let directory = tempfile::tempdir().unwrap();
        let command =
            mix_events::command(Request::UserRemove(mix_events::v1::UserRemoveRequest {}));
        let unconfirmed = mix_events::Fault::failed(
            Code::ConfirmationRequired,
            "there is no terminal to ask on and `--yes` was not given",
            None,
        );
        let recorded = Recorded::default();

        let exit = run(
            command,
            &recorded.view(),
            &client::Daemon::in_dir(directory.path()),
            Some(unconfirmed),
        )
        .await;

        assert_eq!(exit, ExitCode::from(2));
        let document = recorded.document();
        assert_eq!(document.command, "user remove");
        assert_eq!(document.exit, mix_events::exit::USAGE);
        assert_eq!(document.problems[0].code(), Code::ConfirmationRequired);
        assert_eq!(
            document.problems[0].title,
            "The command needs a confirmation"
        );
        assert!(document.changes.is_empty());
    }

    #[tokio::test]
    async fn an_unconfirmed_removal_exits_as_a_usage_error_without_json_too() {
        let directory = tempfile::tempdir().unwrap();
        let command =
            mix_events::command(Request::UserRemove(mix_events::v1::UserRemoveRequest {}));
        let view = View {
            json: Json::Off,
            verbose: 0,
            quiet: true,
            exit: Exit::default(),
            departure: mix_render::Departure::default(),
            preview: false,
        };

        let exit = run(
            command,
            &view,
            &client::Daemon::in_dir(directory.path()),
            Some(mix_events::Fault::failed(
                Code::ConfirmationRequired,
                "the answer was not yes",
                None,
            )),
        )
        .await;

        assert_eq!(exit, ExitCode::from(2));
    }
}
