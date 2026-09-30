#![no_main]

use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

use mix_cli::render::human::Human;
use mix_events::Normalize;
use mix_events::v1::Envelope;
use mix_shell::render::Render;
use prost::Message;

fn consume(envelopes: &[Envelope]) {
    let _ = mix_events::validate(envelopes);
    let mut human = Human::new(Arc::new(mix_ui::Silent)).level(mix_events::Detail::Trace);
    for envelope in envelopes {
        let json = serde_json::to_string(envelope).expect("an envelope has a JSON form");
        let read: Envelope = serde_json::from_str(&json).expect("its JSON form reads back");
        assert_eq!(&read, envelope, "{json}");
        human.envelope(envelope.clone());
    }
}

fuzz_target!(|data: &[u8]| {
    let mut wire = data;
    let mut envelopes = Vec::new();
    while let Ok(mut envelope) = Envelope::decode_length_delimited(&mut wire) {
        envelope.normalize();
        envelopes.push(envelope);
    }
    consume(&envelopes);
    if let Ok(captured) = mix_events::capture::read(data) {
        consume(&captured.envelopes);
    }
});
