use std::io::IsTerminal;

fn styled(code: &str, symbol: &str, message: &str, is_terminal: bool) -> String {
    if is_terminal && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{symbol}\x1b[0m {message}")
    } else {
        format!("{symbol} {message}")
    }
}

fn sentence_case(message: &str) -> String {
    let capitalized = match message.chars().next() {
        Some(c) if c.is_ascii_lowercase() => {
            c.to_ascii_uppercase().to_string() + &message[c.len_utf8()..]
        }
        _ => message.to_string(),
    };
    if capitalized.contains('\n') || capitalized.ends_with(['.', '!', '?']) {
        capitalized
    } else {
        capitalized + "."
    }
}

pub fn ok(message: impl std::fmt::Display) {
    let message = sentence_case(&message.to_string());
    println!(
        "{}",
        styled("32", "✓", &message, std::io::stdout().is_terminal())
    );
}

pub fn fail(message: impl std::fmt::Display) {
    let message = sentence_case(&message.to_string());
    eprintln!(
        "{}",
        styled("31", "✗", &message, std::io::stderr().is_terminal())
    );
}

pub fn info(message: impl std::fmt::Display) {
    eprintln!("{}", sentence_case(&message.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_case_capitalizes_a_lowercase_first_letter() {
        assert_eq!(
            sentence_case("mix must run as root"),
            "Mix must run as root."
        );
    }

    #[test]
    fn sentence_case_leaves_a_path_led_message_untouched() {
        assert_eq!(sentence_case("/nix: missing"), "/nix: missing.");
    }

    #[test]
    fn sentence_case_does_not_double_the_trailing_period() {
        assert_eq!(
            sentence_case("already ends with a period."),
            "Already ends with a period."
        );
    }

    #[test]
    fn sentence_case_preserves_embedded_newlines() {
        assert_eq!(
            sentence_case("On WSL2, do X.\n\nmix requires systemd."),
            "On WSL2, do X.\n\nmix requires systemd."
        );
    }

    #[test]
    fn sentence_case_does_not_append_a_period_to_a_multiline_command() {
        assert_eq!(
            sentence_case("remove it:\n  sudo rm -rf /nix"),
            "Remove it:\n  sudo rm -rf /nix"
        );
    }
}
