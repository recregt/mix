use std::sync::Arc;

use mix_events::Outbox;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use super::context::Request;
use super::sink::Shared;

pub(crate) struct Delivery {
    close: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

fn pass(outbox: &Outbox, render: &Shared) {
    let mut render = render
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for envelope in outbox.drain() {
        render.envelope(envelope);
    }
}

pub(crate) fn open(render: &Shared) -> (Request, Delivery) {
    let id = crate::request::context::request_id();
    let notify = Arc::new(Notify::new());
    let wake = Arc::clone(&notify);
    let outbox = Arc::new(Outbox::new(id.clone(), move || wake.notify_one()));
    let (close, mut closed) = oneshot::channel();
    let delivered = Arc::clone(&outbox);
    let render = Arc::clone(render);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut closed => {
                    pass(&delivered, &render);
                    return;
                }
                () = notify.notified() => pass(&delivered, &render),
            }
        }
    });
    (Request { id, outbox }, Delivery { close, task })
}

impl Delivery {
    pub(crate) async fn finish(self) {
        let _ = self.close.send(());
        let _ = self.task.await;
    }
}
