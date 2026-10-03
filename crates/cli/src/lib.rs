mod args;
mod client;
mod controls;
mod request;
mod session;

use std::process::ExitCode;

use args::{Args, Color, Output};

pub async fn run() -> ExitCode {
    let args = Args::parse_with_color();
    let request = request::from_args(&args.command, |name| std::env::var(name).ok());
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
        args.draws_progress(),
    );
    session::run(request, &view).await
}
