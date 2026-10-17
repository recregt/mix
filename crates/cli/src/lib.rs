mod args;
mod client;
mod controls;
mod env;
mod request;
mod session;

use std::process::ExitCode;

use args::{Args, Color, Command};

pub async fn run() -> ExitCode {
    let args = Args::parse_with_color();
    let environment = env::Environment::read();
    let command = request::command_of(&args.command, args.dry_run(), &environment);
    let view = || mix_render::View {
        json: if args.json {
            mix_render::Json::Stdout
        } else {
            mix_render::Json::Off
        },
        verbose: args.verbose,
        quiet: args.quiet,
        exit: mix_render::Exit::default(),
        departure: mix_render::Departure::default(),
    };
    mix_render::start(
        match args.color {
            Color::Auto => mix_render::Color::Auto,
            Color::Always => mix_render::Color::Always,
            Color::Never => mix_render::Color::Never,
        },
        args.draws_progress(environment.ci),
    );
    let interactive = !args.json && mix_render::interactive();
    let socket = std::path::Path::new(mix_rpc::SOCKET_PATH);
    let first = view();
    let exit = match request::confirmation(&args.command, interactive, mix_render::confirm) {
        request::Confirmation::Given => session::run(command, &first, socket, None).await,
        request::Confirmation::Refused(fault) => {
            session::run(command, &first, socket, Some(fault)).await
        }
        request::Confirmation::AfterThePlan => {
            let plan = request::command_of(&args.command, true, &environment);
            let planned = session::run(plan, &view(), socket, None).await;
            if planned != ExitCode::SUCCESS {
                return planned;
            }
            let answer = (!mix_render::confirm(request::PURGE_QUESTION))
                .then(|| request::refusal("the answer was not yes"));
            session::run(command, &first, socket, answer).await
        }
    };
    if exit == ExitCode::SUCCESS
        && interactive
        && request::leaves_the_machine(&args.command, first.departure.others())
        && mix_render::confirm("Remove Nix from this machine as well?")
    {
        let purge = request::command_of(
            &Command::Purge {
                yes: true,
                dry_run: false,
            },
            false,
            &environment,
        );
        return session::run(purge, &view(), socket, None).await;
    }
    exit
}
