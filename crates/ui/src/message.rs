//! Deciding how a message is written, before anything is printed.
//!
//! Every line the tool prints is a mix of prose and names — paths, units, packages, commands —
//! and the two want opposite treatment. Prose reads better capitalized and closed with a full
//! stop; a name is a string the user has to be able to copy, search for or type back, so
//! changing its first letter or gluing a period onto its end is at best noise and at worst
//! wrong: `nixbld1` is not `Nixbld1`, and `/nix/store.` is not a path.
//!
//! So neither decision is taken over the message as a whole. The leading word decides whether
//! the first letter is raised, the final line decides whether a full stop is added, and both are
//! judged by shape: what a word looks like is what says whether it is a word at all.

/// Names the project writes in lower case. Capitalizing one of these turns it into something
/// that cannot be searched for in the docs, or into a different name altogether.
const LOWERCASE_NAMES: [&str; 8] = [
    "mix",
    "nix",
    "nixpkgs",
    "nixbld",
    "nix-daemon",
    "home-manager",
    "systemd",
    "sudo",
];

/// Longest a word can be and still be read as one. Past this, a token carrying digits is a hash,
/// an id or a version rather than something a full stop belongs after.
const MAX_WORD: usize = 16;

/// Dropped from the ends of a word before its shape is judged: they belong to the sentence
/// around it, not to the word itself.
const WRAPPERS: [char; 8] = ['\'', '"', '(', ')', '[', ']', '{', '}'];

/// Punctuation that already closes a line: either it ends the sentence, or it deliberately
/// leaves it open for the line that follows.
const CLOSERS: [char; 9] = ['.', '!', '?', ':', ';', ',', '…', ')', ']'];

/// A code span is drawn in the same accent the rest of the UI uses, and the backticks it was
/// written with are dropped: what is left on screen is the command, ready to be copied.
const CODE: &str = "\u{1b}[36m";
const UNCODE: &str = "\u{1b}[39m";

/// Appends `message` the way it should be read.
///
/// The first letter is raised if the message opens with prose, a full stop is added if it closes
/// with a sentence, continuation lines are indented to sit under the first one, and — when the
/// terminal takes colour — a backticked span is drawn as code instead of as backticks.
///
/// One pass, straight into the caller's buffer: a printed line costs the string it is printed
/// from and nothing else.
pub fn write_message(out: &mut String, message: &str, indent: usize, colour: bool) {
    let terminate = ends_a_sentence(last_line(message));
    let capitalize = opens_with_prose(message);

    let mut lines = message.split('\n').peekable();
    let mut first = true;
    while let Some(line) = lines.next() {
        if !first {
            out.push('\n');
            if !line.is_empty() {
                // Continuation lines sit under the first one, so a message with a hint under it
                // reads as one block instead of as a line and an afterthought.
                for _ in 0..indent {
                    out.push(' ');
                }
            }
        }

        write_line(out, line, first && capitalize, colour);
        if lines.peek().is_none() && terminate {
            out.push('.');
        }
        first = false;
    }
}

/// [`write_message`] into a string of its own, for a caller with nothing to append to.
pub fn polished(message: &str) -> String {
    let mut out = String::with_capacity(message.len() + 1);
    write_message(&mut out, message, 2, false);
    out
}

fn write_line(out: &mut String, line: &str, capitalize: bool, colour: bool) {
    let body = if capitalize {
        let (first, rest) = split_first_char(line);
        // The character was checked to be ASCII before this point, so raising it is one byte.
        out.push(first.to_ascii_uppercase());
        rest
    } else {
        line
    };

    if !colour {
        out.push_str(body);
        return;
    }
    write_code_spans(out, body);
}

/// Draws the backticked spans of a line as code, and everything else as it was written.
///
/// A span that is opened and never closed is left exactly as it arrived: a stray backtick in a
/// message is more likely to be part of the text than a mistake worth repairing on screen.
fn write_code_spans(out: &mut String, line: &str) {
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };

        out.push_str(&rest[..open]);
        out.push_str(CODE);
        out.push_str(&after[..close]);
        out.push_str(UNCODE);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
}

fn split_first_char(line: &str) -> (char, &str) {
    let mut chars = line.chars();
    let first = chars.next().unwrap_or(' ');
    (first, chars.as_str())
}

fn last_line(message: &str) -> &str {
    message.rsplit('\n').next().unwrap_or(message)
}

/// Whether the message opens with a word, rather than with something that has to keep the
/// spelling it arrived in.
fn opens_with_prose(message: &str) -> bool {
    let Some(first) = message.chars().next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    is_word(first_word(message))
}

/// Whether a full stop belongs at the end of this line.
fn ends_a_sentence(line: &str) -> bool {
    // An indented line is a command to copy or a block to read, not a sentence to close.
    if line.starts_with([' ', '\t']) {
        return false;
    }

    let line = line.trim_end();
    if line.is_empty() || line.ends_with(CLOSERS) {
        return false;
    }

    // A line of one token is a label — a path, a unit, a package — and a full stop would read
    // as part of it.
    let Some(last) = line.rsplit(' ').next().filter(|_| line.contains(' ')) else {
        return false;
    };
    closes_a_sentence(last)
}

/// The leading word of a message, up to the whitespace that ends it.
fn first_word(message: &str) -> &str {
    match message.find(char::is_whitespace) {
        Some(end) => &message[..end],
        None => message,
    }
}

/// Drops what belongs to the sentence around a token rather than to the token itself.
fn bare(token: &str) -> &str {
    let token = token.trim_matches(|c| WRAPPERS.contains(&c));
    token.trim_end_matches(|c| CLOSERS.contains(&c))
}

/// Whether a token reads as an ordinary word.
fn is_word(token: &str) -> bool {
    is_plain_word(bare(token))
}

/// Whether a token can carry the full stop that closes a line.
///
/// A word can, and so can a count (`expected 755`) and a pair written with a slash (`uid/gid`) —
/// a path is told apart from a pair by its root. Anything else is a name: a store path, a flag,
/// a unit, a hash, a version, a `key=value`, a backticked command.
fn closes_a_sentence(token: &str) -> bool {
    let token = bare(token);
    if token.is_empty() {
        return false;
    }
    is_number(token) || token.split('/').all(is_plain_word)
}

/// Letters, with the hyphens and apostrophes English puts inside them, and not a name this
/// project spells in lower case.
fn is_plain_word(token: &str) -> bool {
    if token.is_empty() || token.len() > MAX_WORD {
        return false;
    }
    // A hyphen or an apostrophe belongs inside a word; leading, it is a flag (`--build`).
    let bytes = token.as_bytes();
    if !bytes[0].is_ascii_alphabetic() || !bytes[bytes.len() - 1].is_ascii_alphabetic() {
        return false;
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphabetic() || *b == b'-' || *b == b'\'')
    {
        return false;
    }
    // A name and what is built on it: `mix`, but also `mix-users`, `nix-daemon`, `nixbld`.
    !LOWERCASE_NAMES.contains(&token)
        && !LOWERCASE_NAMES.contains(&token.split('-').next().unwrap_or(token))
}

/// Whether a token is a plain count, e.g. the mode a directory was found with. A token mixing
/// digits with letters is an id, not a number.
fn is_number(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sentence_is_capitalized_and_closed() {
        assert_eq!(polished("nothing to install"), "Nothing to install.");
    }

    #[test]
    fn an_existing_full_stop_is_not_doubled() {
        assert_eq!(polished("already ends here."), "Already ends here.");
    }

    /// The names the tool prints most are the ones a user has to type back.
    #[test]
    fn a_path_is_left_exactly_as_it_is() {
        assert_eq!(polished("/nix/store"), "/nix/store");
    }

    #[test]
    fn a_unit_name_keeps_its_own_spelling() {
        assert_eq!(polished("nix-daemon.service"), "nix-daemon.service");
    }

    #[test]
    fn a_user_name_is_neither_capitalized_nor_closed() {
        assert_eq!(polished("nixbld1"), "nixbld1");
    }

    #[test]
    fn a_package_name_is_left_alone() {
        assert_eq!(polished("ripgrep-14.1.1"), "ripgrep-14.1.1");
    }

    /// `mix` and `systemd` are spelled in lower case wherever the tool talks about them.
    #[test]
    fn a_name_the_project_spells_in_lower_case_stays_that_way() {
        assert_eq!(polished("mix must run as root"), "mix must run as root.");
        assert_eq!(
            polished("systemd doesn't appear to be active"),
            "systemd doesn't appear to be active."
        );
    }

    /// The group and the unit are named after the tool, and are just as much names as it is.
    #[test]
    fn a_name_built_on_one_of_them_stays_that_way_too() {
        assert_eq!(
            polished("mix-users: group is missing"),
            "mix-users: group is missing."
        );
        assert_eq!(
            polished("nix-daemon is not running"),
            "nix-daemon is not running."
        );
    }

    #[test]
    fn prose_about_a_name_is_still_capitalized() {
        assert_eq!(
            polished("running `mix bootstrap` first would fix this"),
            "Running `mix bootstrap` first would fix this."
        );
    }

    #[test]
    fn a_sentence_ending_in_a_path_is_left_open() {
        assert_eq!(
            polished("already locked: /run/mix.lock"),
            "Already locked: /run/mix.lock"
        );
    }

    #[test]
    fn a_sentence_ending_in_a_command_is_left_open() {
        assert_eq!(
            polished("run `mix repair` to reconcile the drift"),
            "Run `mix repair` to reconcile the drift."
        );
        assert_eq!(
            polished("reconcile the drift with `mix repair`"),
            "Reconcile the drift with `mix repair`"
        );
    }

    #[test]
    fn a_sentence_ending_in_a_flag_is_left_open() {
        assert_eq!(
            polished("re-run the install with --build"),
            "Re-run the install with --build"
        );
    }

    #[test]
    fn a_sentence_ending_in_a_hash_is_left_open() {
        assert_eq!(
            polished("sha256 mismatch: got 8f14e45fceea167a5a36dedd4bea2543"),
            "sha256 mismatch: got 8f14e45fceea167a5a36dedd4bea2543"
        );
    }

    /// A count in brackets closes the line by itself; a full stop after it reads as a fragment.
    #[test]
    fn a_parenthesised_tail_closes_the_line_by_itself() {
        assert_eq!(polished("Filesystem (7 checks)"), "Filesystem (7 checks)");
    }

    #[test]
    fn a_line_ending_in_a_colon_is_left_open() {
        assert_eq!(
            polished("the packages it would build:"),
            "The packages it would build:"
        );
    }

    /// A pair written with a slash is two words; a path is told apart from it by its root.
    #[test]
    fn a_pair_written_with_a_slash_still_closes_the_line() {
        assert_eq!(
            polished("nixbld1: user is missing or has the wrong uid/gid"),
            "nixbld1: user is missing or has the wrong uid/gid."
        );
    }

    #[test]
    fn a_number_is_closed_like_any_other_word() {
        assert_eq!(
            polished("/nix/store: mode is 700, expected 755"),
            "/nix/store: mode is 700, expected 755."
        );
    }

    /// Only the last line decides the full stop, so a hint under a failure is closed on its own
    /// terms rather than the first line's.
    #[test]
    fn the_last_line_decides_the_full_stop() {
        assert_eq!(
            polished("not bootstrapped yet\nrun it first"),
            "Not bootstrapped yet\n  run it first."
        );
    }

    /// A block to copy keeps its own indentation, shifted to stay under the text it belongs to.
    #[test]
    fn an_indented_command_is_never_closed() {
        assert_eq!(
            polished("remove it:\n  sudo rm -rf /nix"),
            "Remove it:\n    sudo rm -rf /nix"
        );
    }

    #[test]
    fn continuation_lines_sit_under_the_first_one() {
        assert_eq!(
            polished("it failed.\nTry this instead."),
            "It failed.\n  Try this instead."
        );
    }

    #[test]
    fn a_blank_line_is_left_blank() {
        assert_eq!(
            polished("it failed.\n\nTry this instead."),
            "It failed.\n\n  Try this instead."
        );
    }

    #[test]
    fn a_multi_line_message_keeps_its_embedded_paths() {
        let message = "cannot move /nix/store/pkg-a into place: it is on a different \
                       filesystem than /nix.\n\
                       `mix` stages packages under /nix and moves them into /nix/store with an \
                       atomic rename. Remove any separate mount at /nix/store and retry";

        let result = polished(message);

        assert!(result.starts_with("Cannot move /nix/store/pkg-a into place"));
        assert!(result.ends_with("at /nix/store and retry."));
        assert!(result.contains("\n  `mix` stages"));
    }

    #[test]
    fn a_code_span_is_drawn_as_code_when_the_terminal_takes_colour() {
        let mut out = String::new();
        write_message(&mut out, "run `mix repair`", 2, true);

        assert_eq!(out, format!("Run {CODE}mix repair{UNCODE}"));
    }

    #[test]
    fn a_span_that_is_never_closed_is_left_as_it_was_written() {
        let mut out = String::new();
        write_message(&mut out, "a stray ` backtick", 2, true);

        assert_eq!(out, "A stray ` backtick.");
    }

    #[test]
    fn backticks_are_kept_when_there_is_no_colour_to_draw_them_in() {
        assert_eq!(polished("run `mix repair` now"), "Run `mix repair` now.");
    }

    #[test]
    fn an_empty_message_stays_empty() {
        assert_eq!(polished(""), "");
    }
}
