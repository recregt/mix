//! Audit entries written to the systemd journal with its native protocol: one for every step
//! and action a request finishes, one for every warning, and one for how the request ended.
//!
//! Note: Without a running journald the entry is dropped. The journal is a record for people,
//! never what a test checks.

use std::collections::HashMap;
use std::os::unix::net::UnixDatagram;

use mix_events::NodeId;
use mix_events::v1::{Envelope, Severity, Status, envelope, node_started};

/// Datagram socket journald reads native protocol entries from.
const SOCKET: &str = "/run/systemd/journal/socket";

fn encode(fields: &[(&str, String)]) -> Vec<u8> {
    let mut entry = Vec::new();
    for (name, value) in fields {
        entry.extend_from_slice(name.as_bytes());
        if value.contains('\n') {
            entry.push(b'\n');
            entry.extend_from_slice(&(value.len() as u64).to_le_bytes());
        } else {
            entry.push(b'=');
        }
        entry.extend_from_slice(value.as_bytes());
        entry.push(b'\n');
    }
    entry
}

pub fn record(fields: &[(&str, String)]) {
    let _ = UnixDatagram::unbound().and_then(|socket| socket.send_to(&encode(fields), SOCKET));
}

pub type Entry = Vec<(&'static str, String)>;

enum Node {
    Step(String),
    Rollback(NodeId),
    Action { operation: String, subject: String },
    Other,
}

pub struct Steps {
    who: String,
    command: String,
    dry_run: bool,
    nodes: HashMap<NodeId, (NodeId, Node)>,
}

impl Steps {
    pub fn new(who: &str, command: &str) -> Self {
        Self {
            who: who.to_string(),
            command: command.to_string(),
            dry_run: false,
            nodes: HashMap::new(),
        }
    }

    pub fn entries(&mut self, envelope: &Envelope) -> Vec<Entry> {
        match envelope.event.as_ref() {
            Some(envelope::Event::NodeStarted(started)) => {
                let node = match &started.kind {
                    Some(node_started::Kind::Command(command)) => {
                        self.dry_run = command.dry_run;
                        Node::Other
                    }
                    Some(node_started::Kind::Step(_)) => Node::Step(started.key.clone()),
                    Some(node_started::Kind::Rollback(rollback)) => Node::Rollback(rollback.undoes),
                    Some(node_started::Kind::Action(action)) => Node::Action {
                        operation: words(action.operation().as_str_name(), "OPERATION_"),
                        subject: action.subject.clone(),
                    },
                    _ => Node::Other,
                };
                self.nodes.insert(started.id, (started.parent, node));
                Vec::new()
            }
            Some(envelope::Event::NodeFinished(finished)) if !self.dry_run => {
                let status = finished.status();
                let mut entry = match self.nodes.get(&finished.id) {
                    Some((_, Node::Step(key))) => self.entry(
                        &envelope.request,
                        format!("{key} {}", status_words(status)),
                        status,
                    ),
                    Some((parent, Node::Action { operation, subject })) => {
                        let mut entry = self.entry(
                            &envelope.request,
                            format!("{operation} {subject}: {}", status_words(status)),
                            status,
                        );
                        entry.push(("MIX_OPERATION", operation.clone()));
                        entry.push(("MIX_SUBJECT", subject.clone()));
                        if self.undoing(*parent) {
                            entry.push(("MIX_UNDO", "1".to_string()));
                        }
                        entry
                    }
                    _ => return Vec::new(),
                };
                if let Some(step) = self.step_of(finished.id) {
                    entry.push(("MIX_STEP", step));
                }
                vec![entry]
            }
            Some(envelope::Event::Diagnostic(found)) if found.severity() == Severity::Warning => {
                let mut entry =
                    self.entry(&envelope.request, found.message.clone(), Status::Failed);
                entry.push(("MIX_CODE", mix_events::code::name(found.code()).to_string()));
                vec![entry]
            }
            _ => Vec::new(),
        }
    }

    fn entry(&self, request: &str, message: String, status: Status) -> Entry {
        let succeeded = matches!(status, Status::Succeeded | Status::AlreadySatisfied);
        vec![
            ("MESSAGE", message),
            ("PRIORITY", if succeeded { "6" } else { "4" }.to_string()),
            ("SYSLOG_IDENTIFIER", "mix-daemon".to_string()),
            ("MIX_USER", self.who.clone()),
            ("MIX_COMMAND", self.command.clone()),
            ("MIX_REQUEST", request.to_string()),
            ("MIX_STATUS", status_words(status)),
        ]
    }

    fn ancestors(&self, id: NodeId) -> impl Iterator<Item = &Node> {
        std::iter::successors(self.nodes.get(&id), |(parent, _)| self.nodes.get(parent))
            .map(|(_, node)| node)
    }

    fn undoing(&self, id: NodeId) -> bool {
        self.ancestors(id)
            .any(|node| matches!(node, Node::Rollback(_)))
    }

    fn step_of(&self, id: NodeId) -> Option<String> {
        self.ancestors(id).find_map(|node| match node {
            Node::Step(key) => Some(key.clone()),
            Node::Rollback(undoes) => self.step_of(*undoes),
            _ => None,
        })
    }
}

fn words(name: &str, prefix: &str) -> String {
    name.trim_start_matches(prefix)
        .to_ascii_lowercase()
        .replace('_', " ")
}

pub fn status_words(status: Status) -> String {
    words(status.as_str_name(), "STATUS_")
}

#[cfg(test)]
mod tests {
    use mix_core::declared::policy::Policy;
    use mix_core::declared::targets::Runtime;
    use mix_core::effect::Digest;
    use mix_core::model::World;
    use mix_core::model::testkit::{Script, drive, requested};
    use mix_core::ops::bootstrap::{Settings, steps};
    use mix_core::run::Runner;
    use mix_events::v1::Command;
    use mix_events::v1::command::Request;
    use mix_events::{ROOT, Start};

    use super::*;

    fn bootstrap(start: Start, script: &Script) -> Vec<Entry> {
        let mut world = World::default();
        world.with_file(
            mix_core::declared::paths::RUNNING_PROGRAM,
            b"mix-daemon",
            0o755,
            (0, 0),
        );
        let settings = Settings {
            policy: Policy::new(None, None).unwrap(),
            user: None,
            force: false,
            runtime: Runtime {
                url: "https://mirror.internal/nix.tar.xz".into(),
                sha256: Digest([7; 32]),
                size: 1,
            },
            request: "request".into(),
        };
        let run = drive(
            &mut world,
            Runner::new(ROOT, steps(&settings)),
            start,
            script,
        );
        let mut steps = Steps::new("alice", "bootstrap");
        run.stream
            .iter()
            .flat_map(|envelope| steps.entries(envelope))
            .collect()
    }

    fn field<'a>(entry: &'a Entry, name: &str) -> Option<&'a str> {
        entry
            .iter()
            .find(|(found, _)| *found == name)
            .map(|(_, value)| value.as_str())
    }

    fn a_bootstrap_that_fails_at_its_second_action() -> Vec<Entry> {
        bootstrap(
            requested(Request::Bootstrap(Box::default())),
            &Script::failing_at(1),
        )
    }

    #[test]
    fn every_finished_action_step_and_undo_is_an_entry_in_the_order_it_happened() {
        let entries = a_bootstrap_that_fails_at_its_second_action();

        let seen: Vec<(&str, String, &str)> = entries
            .iter()
            .map(|entry| {
                let kind = match (field(entry, "MIX_OPERATION"), field(entry, "MIX_UNDO")) {
                    (Some(_), Some(_)) => "undo",
                    (Some(_), None) => "action",
                    (None, _) => "step",
                };
                let what = match field(entry, "MIX_OPERATION") {
                    Some(operation) => {
                        format!("{operation} {}", field(entry, "MIX_SUBJECT").unwrap())
                    }
                    None => field(entry, "MIX_STEP").unwrap().to_string(),
                };
                (kind, what, field(entry, "MIX_STATUS").unwrap())
            })
            .collect();

        assert_eq!(
            seen,
            [
                ("action", "add group nixbld".to_string(), "succeeded"),
                ("step", "nixbld".to_string(), "succeeded"),
                ("action", "add user nixbld1".to_string(), "failed"),
                ("step", "nixbld1".to_string(), "failed"),
                ("undo", "delete group nixbld".to_string(), "succeeded"),
            ]
        );
    }

    #[test]
    fn every_entry_names_who_ran_which_command_for_which_request() {
        for entry in a_bootstrap_that_fails_at_its_second_action() {
            assert_eq!(field(&entry, "SYSLOG_IDENTIFIER"), Some("mix-daemon"));
            assert_eq!(field(&entry, "MIX_USER"), Some("alice"));
            assert_eq!(field(&entry, "MIX_COMMAND"), Some("bootstrap"));
            assert_eq!(field(&entry, "MIX_REQUEST"), Some("request"));
        }
    }

    #[test]
    fn a_failure_is_logged_as_a_warning_and_anything_else_as_information() {
        let entries = a_bootstrap_that_fails_at_its_second_action();

        assert!(
            entries
                .iter()
                .any(|entry| field(entry, "MIX_STATUS") == Some("failed"))
        );
        for entry in entries {
            let priority = if field(&entry, "MIX_STATUS") == Some("failed") {
                "4"
            } else {
                "6"
            };
            assert_eq!(field(&entry, "PRIORITY"), Some(priority), "{entry:?}");
        }
    }

    #[test]
    fn a_dry_run_changes_nothing_and_records_no_step() {
        let dry_run = Start::command(
            "bootstrap",
            Command {
                dry_run: true,
                request: Some(Request::Bootstrap(Box::default())),
                ..Command::default()
            },
        );

        assert!(bootstrap(dry_run, &Script::default()).is_empty());
    }

    #[test]
    fn a_plain_value_is_written_as_name_equals_value() {
        assert_eq!(
            encode(&[
                ("MIX_USER", "alice".to_string()),
                ("PRIORITY", "6".to_string())
            ]),
            b"MIX_USER=alice\nPRIORITY=6\n"
        );
    }

    #[test]
    fn a_value_with_a_newline_is_written_with_its_length() {
        let mut expected = b"MESSAGE\n".to_vec();
        expected.extend_from_slice(&3u64.to_le_bytes());
        expected.extend_from_slice(b"a\nb\n");

        assert_eq!(encode(&[("MESSAGE", "a\nb".to_string())]), expected);
    }
}
