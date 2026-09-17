use std::io::IsTerminal;

use owo_colors::OwoColorize;
use owo_colors::colors::{Green, Red, Yellow};

pub mod activity;
mod progress;

pub use activity::activity_reporter;
pub use progress::{download_reporter, init_tracing, step_observer};

fn colors_enabled(is_terminal: bool) -> bool {
    is_terminal && std::env::var_os("NO_COLOR").is_none()
}

fn looks_like_an_identifier(message: &str) -> bool {
    message.starts_with('/') || message.ends_with(".service") || message.ends_with(".socket")
}

/// Capitalizes and terminates a message in place: every line printed goes through this, and it
/// only ever needs the string the caller already rendered.
pub(crate) fn sentence_case(message: impl Into<String>) -> String {
    let mut message: String = message.into();

    let capitalize = message
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase())
        && !looks_like_an_identifier(&message);
    if capitalize {
        // The first character is ASCII, so this is a one-byte in-place uppercase.
        message[..1].make_ascii_uppercase();
    }

    if !message.contains('\n') && !message.ends_with(['.', '!', '?']) {
        message.push('.');
    }
    message
}

pub fn ok(message: impl std::fmt::Display) {
    let message = sentence_case(message.to_string());
    if colors_enabled(std::io::stdout().is_terminal()) {
        println!("{} {message}", "✓".fg::<Green>());
    } else {
        println!("✓ {message}");
    }
}

pub fn fail(message: impl std::fmt::Display) {
    let message = sentence_case(message.to_string());
    if colors_enabled(std::io::stderr().is_terminal()) {
        eprintln!("{} {message}", "✗".fg::<Red>());
    } else {
        eprintln!("✗ {message}");
    }
}

/// Reports something that was deliberately left alone, e.g. a package that is already installed.
pub fn skipped(message: impl std::fmt::Display) {
    let message = sentence_case(message.to_string());
    if colors_enabled(std::io::stderr().is_terminal()) {
        eprintln!("{} {message}", "•".fg::<Yellow>());
    } else {
        eprintln!("• {message}");
    }
}

pub fn info(message: impl std::fmt::Display) {
    eprintln!("{}", sentence_case(message.to_string()));
}

pub fn header(message: impl std::fmt::Display) {
    eprintln!("{message}:");
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

    #[test]
    fn sentence_case_preserves_the_case_of_a_socket_unit_name() {
        assert_eq!(sentence_case("nix-daemon.socket"), "nix-daemon.socket.");
    }

    #[test]
    fn sentence_case_preserves_the_case_of_a_service_unit_name() {
        assert_eq!(sentence_case("nix-daemon.service"), "nix-daemon.service.");
    }

    #[test]
    fn sentence_case_capitalizes_a_cancelled_command_message() {
        assert_eq!(
            sentence_case("command `useradd nixbld1` was interrupted"),
            "Command `useradd nixbld1` was interrupted."
        );
    }

    #[test]
    fn sentence_case_capitalizes_the_interrupted_message() {
        assert_eq!(
            sentence_case("interrupted; rolled back any partially applied changes"),
            "Interrupted; rolled back any partially applied changes."
        );
    }

    #[test]
    fn sentence_case_capitalizes_a_cross_device_store_message_without_touching_the_embedded_path() {
        let message = "cannot move /nix/store/pkg-a into place: it is on a different \
                        filesystem than /nix.\n\
                        `mix` stages packages under /nix and moves them into /nix/store with an \
                        atomic rename, which requires both to be on the same filesystem. Remove \
                        any separate mount at /nix/store (e.g. a custom fstab entry) and retry.";

        let result = sentence_case(message);

        assert!(result.starts_with("Cannot move /nix/store/pkg-a into place"));
        assert!(result.ends_with("and retry."));
        assert!(!result.ends_with("retry.."));
    }
}
