mod controls;
mod env;
mod journal;
mod notify;
mod serve;
mod worker;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::io::Write as _;
use std::process::ExitCode;

const SERVE: &str = "serve";
const SERVE_STDIN: &str = "serve-stdin";

#[allow(clippy::disallowed_methods)]
fn home_files(request: &str) -> ExitCode {
    match mix_home::serve_stdio(request) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let _ = writeln!(std::io::stderr(), "{failure:?}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    mix_ui::report(
        mix_ui::Severity::Error,
        &mix_ui::Report::new(&mix_ui::phrase!(
            "`mix-daemon` is started by `mix` and systemd, not by hand"
        )),
    );
    ExitCode::from(2)
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [SERVE] => serve::serve().await,
        [SERVE_STDIN] => worker::serve_stdin().await,
        [command, request] if *command == mix_home::COMMAND => home_files(request),
        _ => usage(),
    }
}
