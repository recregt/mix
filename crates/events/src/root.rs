use std::sync::Arc;

use crate::v1::command::Request;
use crate::v1::{Command, Envelope};
use crate::{Detail, Ending, Fault, Outbox, ROOT, Start, Tree};

pub trait Render: Send {
    fn envelope(&mut self, envelope: Envelope);

    fn detail(&self) -> Detail;
}

pub fn request_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub fn key_of(request: Option<&Request>) -> &'static str {
    match request {
        Some(Request::Bootstrap(_)) => "bootstrap",
        Some(Request::Install(_)) => "install",
        Some(Request::Remove(_)) => "remove",
        Some(Request::Repair(_)) => "repair",
        Some(Request::Doctor(_)) => "doctor",
        Some(Request::Clean(_)) => "clean",
        None => "command",
    }
}

pub fn command(request: Request) -> Command {
    Command {
        mix_version: env!("CARGO_PKG_VERSION").to_string(),
        schema_minor: crate::SCHEMA_MINOR,
        request: Some(request),
    }
}

pub fn fail(command: Command, fault: Fault, render: &mut (impl Render + ?Sized)) {
    let outbox = Arc::new(Outbox::new(request_id(), || {}));
    let mut tree = Tree::new(
        Arc::clone(&outbox),
        Arc::new(|| None),
        Start::command(key_of(command.request.as_ref()), command),
    );
    let ending: Ending = fault.into();
    let _ = tree.finish(ROOT, ending.for_root(false));
    drop(tree);
    for envelope in outbox.drain() {
        render.envelope(envelope);
    }
}
