use std::time::Duration;

use super::*;
use crate::v1::{Command, envelope};

fn header() -> Header {
    Header {
        format: String::new(),
        mix_version: "0.1.0".into(),
        request: "0192-request".into(),
        started: Some(pbjson_types::Timestamp {
            seconds: 1_790_000_000,
            nanos: 5,
        }),
    }
}

fn envelope(seq: u64) -> Envelope {
    Envelope {
        seq,
        request: "0192-request".into(),
        event: Some(envelope::Event::NodeStarted(crate::v1::NodeStarted {
            id: seq,
            parent: 0,
            key: format!("node-{seq}"),
            planned: Vec::new(),
            shielded: false,
            kind: Some(crate::v1::node_started::Kind::Command(Command::default())),
        })),
    }
}

fn written(offsets: &[u64]) -> Vec<u8> {
    let mut capture = Capture::start(Vec::new(), header()).unwrap();
    for (index, millis) in offsets.iter().enumerate() {
        capture
            .record(Duration::from_millis(*millis), &envelope(index as u64 + 1))
            .unwrap();
    }
    capture.into_inner()
}

#[test]
fn a_capture_reads_back_what_was_written() {
    let bytes = written(&[0, 5, 5, 12]);

    let captured = read(bytes.as_slice()).unwrap();

    assert_eq!(captured.header.format, FORMAT);
    assert_eq!(captured.header.request, "0192-request");
    assert_eq!(
        captured.envelopes,
        (1..=4).map(envelope).collect::<Vec<_>>()
    );
}

#[test]
fn the_header_comes_first_and_only_once() {
    let bytes = written(&[0, 1]);
    let text = String::from_utf8(bytes).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();

    let headless = lines[1..].join("\n");
    assert_eq!(read(headless.as_bytes()), Err(Broken::NoHeader));

    lines.push(lines[0]);
    let doubled = lines.join("\n");
    assert_eq!(read(doubled.as_bytes()), Err(Broken::SecondHeader(4)));
}

fn line(entry: Entry) -> String {
    serde_json::to_string(&Line { entry: Some(entry) }).unwrap()
}

fn record_at(millis: u64, seq: u64) -> String {
    line(Entry::Record(Record {
        offset: Some(duration(Duration::from_millis(millis))),
        envelope: Some(envelope(seq)),
    }))
}

#[test]
fn an_offset_never_goes_back() {
    let mut capture = Capture::start(Vec::new(), header()).unwrap();
    capture
        .record(Duration::from_millis(9), &envelope(1))
        .unwrap();

    assert!(
        capture
            .record(Duration::from_millis(3), &envelope(2))
            .is_err()
    );

    let mut stamped = header();
    stamped.format = FORMAT.to_string();
    let lines = [
        line(Entry::Header(stamped)),
        record_at(9, 1),
        record_at(3, 2),
    ]
    .join("\n");
    assert_eq!(read(lines.as_bytes()), Err(Broken::Backwards(3)));
}

#[test]
fn another_format_is_refused() {
    let text = String::from_utf8(written(&[]))
        .unwrap()
        .replace(FORMAT, "mix.capture.v2");

    assert_eq!(
        read(text.as_bytes()),
        Err(Broken::Format("mix.capture.v2".into()))
    );
}

#[test]
fn a_line_that_is_not_a_record_is_named() {
    let text = String::from_utf8(written(&[0])).unwrap() + "{not json\n";

    assert!(matches!(
        read(text.as_bytes()),
        Err(Broken::Unreadable { line: 3, .. })
    ));
}
