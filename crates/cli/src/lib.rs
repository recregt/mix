mod args;
mod client;
mod controls;
mod env;
mod request;
mod session;

use std::process::ExitCode;

use args::{Args, Color};

pub async fn run() -> ExitCode {
    let args = Args::parse_with_color();
    let environment = env::Environment::read();
    let command = request::command_of(&args.command, args.dry_run(), &environment);
    let view = mix_render::View {
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
    session::run(command, &view, std::path::Path::new(mix_rpc::SOCKET_PATH)).await
}
