#![cfg_attr(not(test), deny(clippy::wildcard_enum_match_arm))]

use std::io::{BufRead, Write};
use std::time::Duration;

use crate::v1::Envelope;

#[allow(clippy::all)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/mix.capture.v1.rs"));
    include!(concat!(env!("OUT_DIR"), "/mix.capture.v1.serde.rs"));
}

use v1::{Header, Line, Record, line::Entry};

pub const FORMAT: &str = "mix.capture.v1";

pub struct Capture<W: Write> {
    out: W,
    last: Duration,
}

impl<W: Write> Capture<W> {
    pub fn start(mut out: W, mut header: Header) -> std::io::Result<Self> {
        header.format = FORMAT.to_string();
        write(
            &mut out,
            &Line {
                entry: Some(Entry::Header(header)),
            },
        )?;
        Ok(Self {
            out,
            last: Duration::ZERO,
        })
    }

    pub fn record(&mut self, offset: Duration, envelope: &Envelope) -> std::io::Result<()> {
        if offset < self.last {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a capture offset went back in time",
            ));
        }
        self.last = offset;
        write(
            &mut self.out,
            &Line {
                entry: Some(Entry::Record(Record {
                    offset: Some(duration(self.last)),
                    envelope: Some(envelope.clone()),
                })),
            },
        )
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

fn duration(offset: Duration) -> pbjson_types::Duration {
    pbjson_types::Duration {
        seconds: i64::try_from(offset.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(offset.subsec_nanos()).unwrap_or(0),
    }
}

fn write<W: Write>(out: &mut W, line: &Line) -> std::io::Result<()> {
    let mut text = serde_json::to_vec(line).map_err(std::io::Error::other)?;
    text.push(b'\n');
    out.write_all(&text)?;
    out.flush()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    pub header: Header,
    pub envelopes: Vec<Envelope>,
    pub offsets: Vec<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Broken {
    #[error("line {line} is not a capture record: {reason}")]
    Unreadable { line: usize, reason: String },

    #[error("the capture does not start with its header")]
    NoHeader,

    #[error("the capture is in format {0}, not {FORMAT}")]
    Format(String),

    #[error("line {0} is a second header")]
    SecondHeader(usize),

    #[error("line {0} is a record without an envelope or an offset")]
    Incomplete(usize),

    #[error("line {0} goes back in time")]
    Backwards(usize),
}

pub fn read(input: impl BufRead) -> Result<Captured, Broken> {
    let mut header = None;
    let mut envelopes = Vec::new();
    let mut offsets = Vec::new();
    let mut last = (0i64, 0i32);
    for (index, text) in input.lines().enumerate() {
        let number = index + 1;
        let text = text.map_err(|error| Broken::Unreadable {
            line: number,
            reason: error.to_string(),
        })?;
        if text.trim().is_empty() {
            continue;
        }
        let line: Line = serde_json::from_str(&text).map_err(|error| Broken::Unreadable {
            line: number,
            reason: error.to_string(),
        })?;
        match (line.entry, &header) {
            (Some(Entry::Header(found)), None) => {
                if found.format != FORMAT {
                    return Err(Broken::Format(found.format));
                }
                header = Some(found);
            }
            (Some(Entry::Header(_)), Some(_)) => return Err(Broken::SecondHeader(number)),
            (_, None) => return Err(Broken::NoHeader),
            (Some(Entry::Record(record)), Some(_)) => {
                let (Some(offset), Some(envelope)) = (record.offset, record.envelope) else {
                    return Err(Broken::Incomplete(number));
                };
                let at = (offset.seconds, offset.nanos);
                if at < last {
                    return Err(Broken::Backwards(number));
                }
                last = at;
                let mut envelope = envelope;
                crate::Normalize::normalize(&mut envelope);
                envelopes.push(envelope);
                offsets.push(Duration::new(
                    u64::try_from(offset.seconds).unwrap_or_default(),
                    u32::try_from(offset.nanos).unwrap_or_default(),
                ));
            }
            (None, Some(_)) => return Err(Broken::Incomplete(number)),
        }
    }
    Ok(Captured {
        header: header.ok_or(Broken::NoHeader)?,
        envelopes,
        offsets,
    })
}

#[cfg(test)]
mod tests;
