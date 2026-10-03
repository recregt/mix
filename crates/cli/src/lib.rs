mod args;
mod local;
mod request;
mod session;

pub mod output;

use std::process::ExitCode;

use args::{Args, Color};

pub async fn run() -> ExitCode {
    let args = Args::parse_with_color();
    mix_ui::set_color(match args.color {
        Color::Auto => mix_ui::ColorChoice::Auto,
        Color::Always => mix_ui::ColorChoice::Always,
        Color::Never => mix_ui::ColorChoice::Never,
    });
    let Some(request) = request::from_args(&args.command, |name| std::env::var(name).ok()) else {
        return local::run(&args);
    };
    let view = output::View {
        output: args.output,
        events_file: args.events_file.clone(),
        verbose: args.verbose,
        quiet: args.quiet,
        exit: output::Exit::default(),
    };
    mix_ui::init(args.draws_progress());
    session::run(request, &view).await
}
