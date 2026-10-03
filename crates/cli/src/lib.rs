mod args;
mod client;
mod controls;
mod env;
mod request;
mod session;

use std::process::ExitCode;

use args::{Args, Color, Output};

pub async fn run() -> ExitCode {
    let args = Args::parse_with_color();
    let environment = env::Environment::read();
    let request = request::from_args(&args.command, &environment);
    let view = mix_render::View {
        format: match args.output {
            Output::Human => mix_render::Format::Human,
            Output::Json => mix_render::Format::Json,
        },
        events_file: args.events_file.clone(),
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
    session::run(request, &view, std::path::Path::new(mix_rpc::SOCKET_PATH)).await
}
