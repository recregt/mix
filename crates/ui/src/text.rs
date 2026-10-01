pub const IMPERATIVES: &[&str] = &[
    "check",
    "install",
    "make",
    "pass",
    "press",
    "recreate",
    "reinstall",
    "remove",
    "report",
    "run",
    "see",
    "turn",
    "uninstall",
    "update",
    "upgrade",
    "use",
    "wait",
];

const INLINE: usize = std::mem::size_of::<String>() - 2;

#[derive(Debug, Clone)]
enum Text {
    Static(&'static str),
    Inline { bytes: [u8; INLINE], len: u8 },
    Owned(String),
}

#[derive(Debug, Clone)]
pub struct Phrase(Text);

impl PartialEq for Phrase {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Phrase {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note(Phrase);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help(Phrase);

impl Phrase {
    #[doc(hidden)]
    pub fn checked_static(text: &'static str) -> Self {
        Self(Text::Static(text))
    }

    #[doc(hidden)]
    pub fn checked_owned(text: String) -> Self {
        Self(Text::Owned(text))
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            Text::Static(text) => text,
            Text::Inline { bytes, len } => {
                std::str::from_utf8(&bytes[..usize::from(*len)]).unwrap_or_default()
            }
            Text::Owned(text) => text,
        }
    }
}

impl Note {
    #[doc(hidden)]
    pub fn checked(phrase: Phrase) -> Self {
        Self(phrase)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Help {
    #[doc(hidden)]
    pub fn checked(phrase: Phrase) -> Self {
        Self(phrase)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for Phrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

const fn contains(text: &[u8], pattern: &[u8]) -> bool {
    let mut start = 0;
    while start + pattern.len() <= text.len() {
        let mut at = 0;
        while at < pattern.len() && text[start + at] == pattern[at] {
            at += 1;
        }
        if at == pattern.len() {
            return true;
        }
        start += 1;
    }
    false
}

const fn ascii(text: &[u8]) -> bool {
    let mut at = 0;
    while at < text.len() {
        if !text[at].is_ascii() {
            return false;
        }
        at += 1;
    }
    true
}

const fn first_word(text: &[u8]) -> &[u8] {
    let mut end = 0;
    while end < text.len() && text[end].is_ascii_lowercase() {
        end += 1;
    }
    text.split_at(end).0
}

const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut at = 0;
    while at < a.len() {
        if a[at] != b[at] {
            return false;
        }
        at += 1;
    }
    true
}

pub const fn starts_with_imperative(text: &str) -> bool {
    let word = first_word(text.as_bytes());
    let mut index = 0;
    while index < IMPERATIVES.len() {
        if same(word, IMPERATIVES[index].as_bytes()) {
            return true;
        }
        index += 1;
    }
    false
}

pub const fn is_phrase(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && ascii(bytes)
        && !bytes[0].is_ascii_uppercase()
        && bytes[bytes.len() - 1] != b'.'
        && !contains(bytes, b"; ")
        && !contains(bytes, b". ")
}

pub const fn is_help(text: &str) -> bool {
    is_phrase(text) && starts_with_imperative(text)
}

pub const fn is_note(text: &str) -> bool {
    is_phrase(text) && !starts_with_imperative(text)
}

pub const fn is_sentences(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && ascii(bytes)
        && !bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1] == b'.'
        && !contains(bytes, b"; ")
}

pub const fn is_fragment(text: &str) -> bool {
    let bytes = text.as_bytes();
    ascii(bytes)
        && !contains(bytes, b"; ")
        && !contains(bytes, b". ")
        && (bytes.is_empty() || bytes[bytes.len() - 1] != b'.')
        && is_plain(text)
}

pub const fn is_plain(text: &str) -> bool {
    !contains(text.as_bytes(), b"{") && !contains(text.as_bytes(), b"}")
}

pub const fn is_sentence(text: &str) -> bool {
    is_sentences(text) && !contains(text.as_bytes(), b". ")
}

#[macro_export]
macro_rules! phrase {
    ($text:literal) => {{
        const _: () = assert!(
            $crate::text::is_phrase($text),
            "a phrase is ASCII, starts lowercase, has no `; ` or `. `, and no final `.`"
        );
        const PLAIN: bool = $crate::text::is_plain($text);
        if PLAIN {
            $crate::text::Phrase::checked_static($text)
        } else {
            $crate::text::Phrase::checked_owned(::std::format!($text))
        }
    }};
    ($text:literal, $($arg:tt)*) => {{
        const _: () = assert!(
            $crate::text::is_phrase($text),
            "a phrase is ASCII, starts lowercase, has no `; ` or `. `, and no final `.`"
        );
        $crate::text::Phrase::checked_owned(format!($text, $($arg)*))
    }};
}

#[macro_export]
macro_rules! write_phrase {
    ($out:expr, $text:literal $(, $($arg:tt)*)?) => {{
        const _: () = assert!(
            $crate::text::is_phrase($text),
            "a phrase is ASCII, starts lowercase, has no `; ` or `. `, and no final `.`"
        );
        ::std::write!($out, $text $(, $($arg)*)?)
    }};
}

fn digits(buffer: &mut [u8; 22], value: u64, base: u64) -> &str {
    let mut at = buffer.len();
    let mut rest = value;
    loop {
        at -= 1;
        buffer[at] = b'0' + (rest % base) as u8;
        rest /= base;
        if rest == 0 {
            break;
        }
    }
    std::str::from_utf8(&buffer[at..]).unwrap_or_default()
}

pub fn decimal(buffer: &mut [u8; 22], value: u64) -> &str {
    digits(buffer, value, 10)
}

pub fn octal(buffer: &mut [u8; 22], value: u64) -> &str {
    digits(buffer, value, 8)
}

#[doc(hidden)]
pub fn joined(parts: &[&str]) -> Phrase {
    let total: usize = parts.iter().map(|part| part.len()).sum();
    if total <= INLINE {
        let mut bytes = [0u8; INLINE];
        let mut at = 0;
        for part in parts {
            bytes[at..at + part.len()].copy_from_slice(part.as_bytes());
            at += part.len();
        }
        return Phrase(Text::Inline {
            bytes,
            len: total as u8,
        });
    }
    let mut text = String::with_capacity(total);
    for part in parts {
        text.push_str(part);
    }
    Phrase(Text::Owned(text))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Around {
    before: &'static str,
    after: &'static str,
}

impl Around {
    #[doc(hidden)]
    pub const fn checked(before: &'static str, after: &'static str) -> Self {
        Self { before, after }
    }

    pub fn around(&self, value: &str) -> Phrase {
        joined(&[self.before, value, self.after])
    }
}

pub const fn starts_parts(first: &str) -> bool {
    first.is_empty() || (is_fragment(first) && is_phrase(first))
}

#[macro_export]
macro_rules! phrase_parts {
    [$first:literal $(, $arg:expr, $next:literal)* $(,)?] => {{
        const _: () = assert!(
            $crate::text::starts_parts($first)
                $(&& $crate::text::is_fragment($next))*,
            "every part of a phrase is a fragment, and the first starts the phrase or is empty"
        );
        $crate::text::joined(&[$first $(, $arg, $next)*])
    }};
}

#[macro_export]
macro_rules! around {
    ($before:literal, $after:literal) => {{
        const _: () = assert!(
            $crate::text::starts_parts($before) && $crate::text::is_fragment($after),
            "the text around a value starts a phrase or is empty, and ends as a fragment"
        );
        $crate::text::Around::checked($before, $after)
    }};
}

#[macro_export]
macro_rules! note_parts {
    [$first:literal $(, $arg:expr, $next:literal)* $(,)?] => {{
        const _: () = assert!(
            !$crate::text::starts_with_imperative($first),
            "a note states a fact; an instruction belongs in a help"
        );
        $crate::text::Note::checked($crate::phrase_parts![$first $(, $arg, $next)*])
    }};
}

#[macro_export]
macro_rules! help_parts {
    [$first:literal $(, $arg:expr, $next:literal)* $(,)?] => {{
        const _: () = assert!(
            $crate::text::is_fragment($first)
                && $crate::text::is_help($first)
                $(&& $crate::text::is_fragment($next))*,
            "a help starts with an instruction, and every part is a phrase fragment"
        );
        $crate::text::Help::checked($crate::text::joined(&[$first $(, $arg, $next)*]))
    }};
}

#[macro_export]
macro_rules! help {
    ($text:literal $(, $($arg:tt)*)?) => {{
        const _: () = assert!(
            $crate::text::starts_with_imperative($text),
            "a help starts with an instruction from `mix_ui::text::IMPERATIVES`"
        );
        $crate::text::Help::checked($crate::phrase!($text $(, $($arg)*)?))
    }};
}

#[macro_export]
macro_rules! note {
    ($text:literal $(, $($arg:tt)*)?) => {{
        const _: () = assert!(
            !$crate::text::starts_with_imperative($text),
            "a note states a fact; an instruction belongs in a help"
        );
        $crate::text::Note::checked($crate::phrase!($text $(, $($arg)*)?))
    }};
}

#[macro_export]
macro_rules! instruction {
    ($text:literal) => {{
        const _: () = assert!(
            $crate::text::is_help($text) && $crate::text::is_plain($text),
            "an instruction is a phrase without braces that starts with a word from `mix_ui::text::IMPERATIVES`"
        );
        $text
    }};
}

#[macro_export]
macro_rules! sentence {
    ($text:literal) => {{
        const _: () = assert!(
            $crate::text::is_sentence($text) && $crate::text::is_plain($text),
            "a sentence is ASCII, starts with a capital or a symbol, has no `; ` or braces, and ends with `.`"
        );
        $text
    }};
}

#[macro_export]
macro_rules! prose {
    ($text:literal) => {{
        const _: () = assert!(
            $crate::text::is_sentences($text) && $crate::text::is_plain($text),
            "prose is ASCII sentences that start with a capital or a symbol, with no `; ` or braces"
        );
        $text
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phrase_is_lowercase_unfinished_and_single() {
        assert!(is_phrase("the change was made but not recorded in git"));
        assert!(is_phrase("`mix repair` records it"));
        assert!(is_phrase("run it again with sudo:\n  sudo mix bootstrap"));
        assert!(is_phrase("{path} isn't a valid events file"));
        assert!(!is_phrase("Run `mix repair` to fix them"));
        assert!(!is_phrase("some checks failed."));
        assert!(!is_phrase("stopped; everything was undone"));
        assert!(!is_phrase("it stopped. Run it again"));
        assert!(!is_phrase("it stopped \u{2014} run it again"));
        assert!(!is_phrase(""));
    }

    #[test]
    fn a_help_is_an_instruction_and_a_note_is_not() {
        assert!(is_help("run `mix repair` to record it"));
        assert!(!is_help("`mix repair` records it"));
        assert!(is_note("`mix` needs it to work"));
        assert!(!is_note("run it again"));
        assert!(!is_help("runaway"));
    }

    #[test]
    fn a_short_join_stays_inline_and_reads_as_a_long_one_does() {
        let short = joined(&["found ", "8", " problems"]);
        assert!(matches!(short.0, Text::Inline { .. }));
        assert_eq!(short.as_str(), "found 8 problems");
        let long = joined(&["/nix/var/nix/profiles", " has the wrong permissions"]);
        assert!(matches!(long.0, Text::Owned(_)));
        assert_eq!(
            long.as_str(),
            "/nix/var/nix/profiles has the wrong permissions"
        );
        assert_eq!(short, Phrase::checked_static("found 8 problems"));
    }

    #[test]
    fn a_number_is_written_without_a_formatter() {
        let mut buffer = [0u8; 22];
        assert_eq!(decimal(&mut buffer, 0), "0");
        assert_eq!(decimal(&mut buffer, 64), "64");
        assert_eq!(decimal(&mut buffer, u64::MAX), "18446744073709551615");
        assert_eq!(octal(&mut buffer, 0o755), "755");
        assert_eq!(octal(&mut buffer, u64::MAX), "1777777777777777777777");
    }

    #[test]
    fn an_explanation_is_made_of_sentences() {
        assert!(is_sentence("`mix` has not been set up for your user."));
        assert!(!is_sentence("one sentence. And another."));
        assert!(is_sentences("One sentence. And another."));
        assert!(!is_sentences("lowercase start."));
        assert!(!is_sentences("No final stop"));
        assert!(!is_sentences("Two parts; joined."));
    }
}
