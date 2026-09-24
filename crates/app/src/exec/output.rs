//! Turning a child process's live byte stream into display lines, cheaply.

use mix_core::ActivityReporter;
use mix_core::nix_log::{Event, NixLog};

/// Bytes a single unterminated line may buffer before it is handed over anyway.
const MAX_LINE: usize = 4 * 1024;

/// Everything a streamed process's output is put through: split into lines, folded into
/// counters, reported as it arrives, and kept — only as far as a failure would be explained
/// from it.
///
/// A process asked for `--log-format internal-json` writes records rather than prose, so the
/// stream is folded into counters as it goes and the diagnostics are decoded out of it; anything
/// that is not a record flows through untouched.
pub struct StreamDrain {
    splitter: LineSplitter,
    /// The raw bytes, kept for as long as they are what a reader would be shown.
    tail: TailBuffer,
    /// The diagnostics decoded out of a structured stream, which is what a reader is shown
    /// instead once nix is writing records.
    decoded: TailBuffer,
    log: NixLog,
}

impl StreamDrain {
    pub fn new(cap: usize) -> Self {
        Self {
            splitter: LineSplitter::new(),
            tail: TailBuffer::new(cap),
            decoded: TailBuffer::new(cap),
            log: NixLog::new(),
        }
    }

    /// Takes a chunk as it was read, reporting whatever it completes.
    pub fn push(&mut self, chunk: &[u8], activity: &dyn ActivityReporter) {
        // The raw bytes are only worth keeping while they are what a failure would be explained
        // with. Once nix is writing records, the tail is discarded in favour of the diagnostics
        // decoded out of it, so copying the stream through it buys nothing: a whole build log
        // used to be copied into a buffer whose contents were then thrown away.
        if !self.log.is_structured() {
            self.tail.extend(chunk);
        }

        let (log, decoded) = (&mut self.log, &mut self.decoded);
        self.splitter
            .push(chunk, |line| report(line, log, decoded, activity));

        if self.log.is_structured() {
            self.tail.discard();
        }
    }

    /// Reports whatever the stream ended on, and hands back what a failure should be explained
    /// with.
    pub fn finish(mut self, activity: &dyn ActivityReporter) -> Vec<u8> {
        let (log, decoded) = (&mut self.log, &mut self.decoded);
        self.splitter
            .finish(|line| report(line, log, decoded, activity));
        activity.clear();

        // Raw records explain nothing to a human, so a structured stream is reported through the
        // diagnostics decoded out of it instead.
        if self.log.is_structured() {
            return self.decoded.into_bytes();
        }
        self.tail.into_bytes()
    }
}

fn report(line: &str, log: &mut NixLog, decoded: &mut TailBuffer, activity: &dyn ActivityReporter) {
    match log.observe(line) {
        Event::Plain(line) => {
            // Only worth a second copy once the raw tail is known to be unreadable.
            if log.is_structured() {
                decoded.push_line(line);
            }
            activity.line(line);
        }
        Event::Message(message) => {
            decoded.push_line(&message);
            activity.line(&message);
        }
        Event::Transient(text) => activity.line(&text),
        Event::Building(derivation) => activity.build_started(derivation),
        Event::Progress => activity.progress(&log.snapshot()),
        Event::Ignored => {}
    }
}

/// Splits a byte stream into display lines as the bytes arrive.
///
/// Build tools redraw in place with carriage returns, so `\r` ends a line here just like `\n`.
/// Lines that are complete within one chunk are handed over as slices of that chunk; only a
/// trailing partial line is copied, so a chunk that ends on a boundary never allocates.
#[derive(Default)]
pub struct LineSplitter {
    partial: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8], mut on_line: impl FnMut(&str)) {
        let mut rest = chunk;

        while let Some(end) = line_end(rest) {
            let (line, tail) = rest.split_at(end);
            rest = &tail[1..];

            if self.partial.is_empty() {
                emit(line, &mut on_line);
            } else {
                self.partial.extend_from_slice(line);
                emit(&self.partial, &mut on_line);
                self.partial.clear();
            }
        }

        if self.partial.len() + rest.len() > MAX_LINE {
            self.partial.extend_from_slice(rest);
            emit(&self.partial, &mut on_line);
            self.partial.clear();
            return;
        }
        self.partial.extend_from_slice(rest);
    }

    pub fn finish(&mut self, mut on_line: impl FnMut(&str)) {
        if !self.partial.is_empty() {
            emit(&self.partial, &mut on_line);
            self.partial.clear();
        }
    }
}

fn line_end(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|b| *b == b'\n' || *b == b'\r')
}

fn emit(line: &[u8], on_line: &mut impl FnMut(&str)) {
    let text = String::from_utf8_lossy(line);
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        on_line(trimmed);
    }
}

/// Keeps the last `cap` bytes of a stream so a failure can be explained without holding on to
/// megabytes of build log. Growth is bounded to `2 * cap` and compaction is amortised O(1).
pub struct TailBuffer {
    buf: Vec<u8>,
    cap: usize,
    truncated: bool,
}

const TRUNCATION_NOTE: &[u8] = b"[earlier output omitted]\n";

impl TailBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
            truncated: false,
        }
    }

    pub fn extend(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        self.compact();
    }

    /// Appends a decoded line, for a stream whose raw bytes are not what a reader wants to see.
    pub fn push_line(&mut self, line: &str) {
        self.buf.extend_from_slice(line.as_bytes());
        self.buf.push(b'\n');
        self.compact();
    }

    /// Gives up what has been kept, and the memory it was kept in, for a stream whose raw bytes
    /// have turned out not to be what a reader will be shown.
    pub fn discard(&mut self) {
        self.buf = Vec::new();
        self.truncated = false;
    }

    fn compact(&mut self) {
        if self.buf.len() > self.cap * 2 {
            self.buf.drain(..self.buf.len() - self.cap);
            self.truncated = true;
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        if !self.truncated {
            return self.buf;
        }
        let mut out = Vec::with_capacity(TRUNCATION_NOTE.len() + self.buf.len());
        out.extend_from_slice(TRUNCATION_NOTE);
        out.extend_from_slice(&self.buf);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut splitter = LineSplitter::new();
        let mut lines = Vec::new();
        for chunk in chunks {
            splitter.push(chunk, |line| lines.push(line.to_string()));
        }
        splitter.finish(|line| lines.push(line.to_string()));
        lines
    }

    #[test]
    fn splits_a_chunk_into_its_lines() {
        assert_eq!(split(&[b"first\nsecond\n"]), ["first", "second"]);
    }

    #[test]
    fn treats_a_carriage_return_as_a_line_end() {
        assert_eq!(split(&[b"redraw\rredrawn\n"]), ["redraw", "redrawn"]);
    }

    #[test]
    fn joins_a_line_split_across_chunks() {
        assert_eq!(
            split(&[b"one hal", b"f and another\n"]),
            ["one half and another"]
        );
    }

    #[test]
    fn hands_over_a_trailing_line_without_a_newline() {
        assert_eq!(split(&[b"no newline here"]), ["no newline here"]);
    }

    #[test]
    fn drops_blank_and_whitespace_only_lines() {
        assert_eq!(split(&[b"\n  \n\r\nreal\n"]), ["real"]);
    }

    #[test]
    fn trims_the_surrounding_whitespace_of_a_line() {
        assert_eq!(split(&[b"  padded  \n"]), ["padded"]);
    }

    #[test]
    fn replaces_invalid_utf8_instead_of_dropping_the_line() {
        assert_eq!(split(&[b"caf\xff\n"]), ["caf\u{fffd}"]);
    }

    #[test]
    fn hands_over_an_overlong_line_instead_of_buffering_it_forever() {
        let mut splitter = LineSplitter::new();
        let mut lines = 0;
        for _ in 0..64 {
            splitter.push(&[b'x'; 1024], |_| lines += 1);
            assert!(splitter.partial.len() <= MAX_LINE);
        }
        assert!(lines > 0);
    }

    #[test]
    fn keeps_no_state_once_every_line_has_been_handed_over() {
        let mut splitter = LineSplitter::new();
        splitter.push(b"done\n", |_| {});
        assert!(splitter.partial.is_empty());
    }

    #[test]
    fn tail_keeps_everything_below_the_cap() {
        let mut tail = TailBuffer::new(16);
        tail.extend(b"short");
        assert_eq!(tail.into_bytes(), b"short");
    }

    #[test]
    fn tail_keeps_the_end_of_an_oversized_stream() {
        let mut tail = TailBuffer::new(8);
        for _ in 0..64 {
            tail.extend(b"0123456789");
        }
        let bytes = tail.into_bytes();
        assert!(bytes.ends_with(b"6789"));
        assert!(bytes.starts_with(TRUNCATION_NOTE));
        assert!(bytes.len() <= TRUNCATION_NOTE.len() + 16);
    }

    /// A structured stream is explained with the diagnostics decoded out of it, so the raw bytes
    /// kept up to that point are not only unused but worth giving the memory back for.
    #[test]
    fn tail_gives_up_what_it_kept_and_the_memory_with_it() {
        let mut tail = TailBuffer::new(8);
        for _ in 0..64 {
            tail.extend(b"0123456789");
        }

        tail.discard();

        assert_eq!(tail.buf.capacity(), 0);
        assert!(tail.into_bytes().is_empty());
    }

    #[test]
    fn tail_never_grows_past_twice_the_cap() {
        let mut tail = TailBuffer::new(32);
        for _ in 0..1000 {
            tail.extend(&[b'y'; 64]);
            assert!(tail.buf.len() <= 64);
        }
    }

    #[derive(Default)]
    struct Builds(std::sync::Mutex<Vec<String>>);

    impl ActivityReporter for Builds {
        fn line(&self, _line: &str) {}
        fn progress(&self, _progress: &mix_core::BuildProgress) {}
        fn clear(&self) {}
        fn build_started(&self, derivation: &str) {
            self.0.lock().unwrap().push(derivation.to_string());
        }
    }

    #[test]
    fn a_build_starting_in_the_stream_is_handed_to_the_reporter() {
        let builds = Builds::default();
        let mut drain = StreamDrain::new(1024);

        drain.push(
            b"@nix {\"action\":\"start\",\"fields\":[\"/nix/store/x-a.drv\",\"\",1,1],\"id\":1,\"level\":3,\"text\":\"building\",\"type\":105}\n",
            &builds,
        );
        drain.finish(&builds);

        assert_eq!(*builds.0.lock().unwrap(), ["/nix/store/x-a.drv"]);
    }
}
