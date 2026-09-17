//! Turning a child process's live byte stream into display lines, cheaply.

/// Bytes a single unterminated line may buffer before it is handed over anyway.
const MAX_LINE: usize = 4 * 1024;

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

    #[test]
    fn tail_never_grows_past_twice_the_cap() {
        let mut tail = TailBuffer::new(32);
        for _ in 0..1000 {
            tail.extend(&[b'y'; 64]);
            assert!(tail.buf.len() <= 64);
        }
    }
}
