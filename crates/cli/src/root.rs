use std::sync::Arc;

use mix_events::v1::{Command, command::Request};
use mix_events::{Fault, Outbox, ROOT, Start, Tree};
use mix_shell::render::Render;

pub fn key_of(request: Option<&Request>) -> &'static str {
    match request {
        Some(Request::Bootstrap(_)) => "bootstrap",
        Some(Request::Install(_)) => "install",
        Some(Request::Remove(_)) => "remove",
        Some(Request::Repair(_)) => "repair",
        Some(Request::Doctor(_)) => "doctor",
        None => "command",
    }
}

pub fn command(request: Request) -> Command {
    Command {
        mix_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_minor: mix_events::SCHEMA_MINOR,
        request: Some(request),
    }
}

pub fn fail(command: Command, fault: Fault, render: &mut impl Render) {
    let outbox = Arc::new(Outbox::new(mix_shell::request_id(), || {}));
    let mut tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command(key_of(command.request.as_ref()), command),
    );
    let ending: mix_events::Ending = fault.into();
    let _ = tree.finish(ROOT, ending.for_root(false));
    drop(tree);
    for envelope in outbox.drain() {
        render.envelope(envelope);
    }
}
