use std::process::ExitCode;

use crate::codes;

pub fn run(code: Option<&str>, list: bool) -> ExitCode {
    if list {
        mix_ui::data(&codes::list_text());
        return ExitCode::SUCCESS;
    }
    let name = code.unwrap_or_default();
    match codes::parse(name) {
        Some(code) => {
            mix_ui::data(&codes::explanation_text(code));
            ExitCode::SUCCESS
        }
        None => {
            mix_ui::report(
                mix_ui::Severity::Error,
                &mix_ui::Report::new(&mix_ui::phrase!("`{name}` isn't a code `mix` uses"))
                    .note(&mix_ui::note!("codes look like `locked` or `network`")),
            );
            ExitCode::from(u8::try_from(mix_events::exit::USAGE).unwrap_or(u8::MAX))
        }
    }
}
