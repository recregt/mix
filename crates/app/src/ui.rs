use std::io::IsTerminal;

fn styled(code: &str, symbol: &str, message: impl std::fmt::Display, is_terminal: bool) -> String {
    if is_terminal && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{symbol}\x1b[0m {message}")
    } else {
        format!("{symbol} {message}")
    }
}

pub fn ok(message: impl std::fmt::Display) {
    println!(
        "{}",
        styled("32", "✓", message, std::io::stdout().is_terminal())
    );
}

pub fn fail(message: impl std::fmt::Display) {
    eprintln!(
        "{}",
        styled("31", "✗", message, std::io::stderr().is_terminal())
    );
}

pub fn info(message: impl std::fmt::Display) {
    eprintln!("{message}");
}
