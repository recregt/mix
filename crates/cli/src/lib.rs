mod accounts;
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
    let unconfirmed = request::unconfirmed(
        &args.command,
        interactive,
        accounts::enrolled,
        mix_render::confirm,
    );
    let socket = std::path::Path::new(mix_rpc::SOCKET_PATH);
    let removal = view();
    let exit = session::run(command, &removal, socket, unconfirmed).await;
    if exit == ExitCode::SUCCESS
        && interactive
        && request::leaves_the_machine(&args.command, removal.exit.removal())
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
