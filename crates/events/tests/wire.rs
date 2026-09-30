use mix_events::v1::{Code, Severity, Status};
use mix_events::v1::{
    Command, Diagnostic, Envelope, InstallRequest, InstallResult, NodeFinished, NodeStarted,
    PackagesDetail, command, diagnostic, envelope, node_finished, node_started,
};
use prost::Message;

fn started() -> Envelope {
    Envelope {
        seq: 1,
        request: "01923f4e-8b5a-7c3d-9e2f-0a1b2c3d4e5f".to_string(),
        event: Some(envelope::Event::NodeStarted(NodeStarted {
            id: u64::MAX,
            parent: 0,
            key: "install".to_string(),
            planned: vec!["change".to_string()],
            shielded: false,
            kind: Some(node_started::Kind::Command(Command {
                mix_version: "0.1.0".to_string(),
                schema_minor: mix_events::SCHEMA_MINOR,
                request: Some(command::Request::Install(InstallRequest {
                    packages: vec!["ripgrep".to_string()],
                })),
            })),
        })),
    }
}

fn finished() -> Envelope {
    Envelope {
        seq: 2,
        request: String::new(),
        event: Some(envelope::Event::NodeFinished(NodeFinished {
            id: 1,
            status: Status::Failed as i32,
            diagnostic: Some(Box::new(Diagnostic {
                code: Code::InvalidPackage as i32,
                severity: Severity::Error as i32,
                node: 2,
                message: String::new(),
                causes: vec![],
                detail: Some(diagnostic::Detail::Packages(PackagesDetail {
                    packages: vec!["hello".to_string()],
                })),
            })),
            exit_code: 1,
            cancellation: 0,
            result: Some(node_finished::Result::Install(InstallResult::default())),
        })),
    }
}

#[test]
fn the_json_form_uses_proto_names_and_keeps_64_bit_ids_exact() {
    let json = serde_json::to_value(started()).unwrap();

    assert_eq!(json["seq"], "1");
    let node = &json["nodeStarted"];
    assert_eq!(node["id"], u64::MAX.to_string());
    assert_eq!(node["command"]["mixVersion"], "0.1.0");
    assert_eq!(node["command"]["install"]["packages"][0], "ripgrep");
}

#[test]
fn the_json_form_names_enums() {
    let json = serde_json::to_value(finished()).unwrap();

    let node = &json["nodeFinished"];
    assert_eq!(node["status"], "STATUS_FAILED");
    assert_eq!(node["diagnostic"]["code"], "CODE_INVALID_PACKAGE");
    assert_eq!(node["diagnostic"]["packages"]["packages"][0], "hello");
}

#[test]
fn every_event_survives_both_encodings() {
    for event in [started(), finished()] {
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<Envelope>(&json).unwrap(), event);

        let bytes = event.encode_to_vec();
        assert_eq!(Envelope::decode(bytes.as_slice()).unwrap(), event);
    }
}

#[test]
fn a_consumer_ignores_fields_it_does_not_know() {
    let json = r#"{"seq":"1","request":"r","nodeProgress":{"id":"1","line":{"text":"x"}},"addedLater":{"a":1}}"#;

    let event: Envelope = serde_json::from_str(json).unwrap();

    assert_eq!(event.seq, 1);
}

#[test]
fn a_consumer_reads_an_enum_value_it_does_not_know_as_unspecified() {
    let json =
        r#"{"seq":"2","request":"r","nodeFinished":{"id":"1","status":"STATUS_ADDED_LATER"}}"#;

    let event: Envelope = serde_json::from_str(json).unwrap();

    let Some(envelope::Event::NodeFinished(finished)) = event.event else {
        panic!("expected a finished node, got {:?}", event.event);
    };
    assert_eq!(finished.status(), Status::Unspecified);
}

#[test]
fn an_envelope_carries_order_and_request_but_no_time() {
    let json = serde_json::to_value(started()).unwrap();

    let mut fields: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(fields, ["nodeStarted", "request", "seq"]);
}

#[test]
fn a_value_from_a_newer_schema_reads_as_unspecified_and_keeps_a_json_form() {
    let mut newer = finished();
    let Some(envelope::Event::NodeFinished(node)) = &mut newer.event else {
        unreachable!()
    };
    node.status = 7247;
    if let Some(diagnostic) = &mut node.diagnostic {
        diagnostic.code = 9001;
        diagnostic.causes.push(Diagnostic {
            severity: 42,
            ..Diagnostic::default()
        });
    }
    let mut decoded = Envelope::decode(newer.encode_to_vec().as_slice()).unwrap();
    assert!(serde_json::to_string(&decoded).is_err());

    mix_events::Normalize::normalize(&mut decoded);

    let json = serde_json::to_string(&decoded).unwrap();
    let Some(envelope::Event::NodeFinished(node)) = &decoded.event else {
        unreachable!()
    };
    assert_eq!(node.status(), Status::Unspecified);
    let diagnostic = node.diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.code(), Code::Unspecified);
    assert_eq!(diagnostic.causes[0].severity(), Severity::Unspecified);
    assert_eq!(serde_json::from_str::<Envelope>(&json).unwrap(), decoded);
}
