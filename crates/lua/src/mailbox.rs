use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bricolo_providers::{EventSink, ProviderEvent};
use tokio::sync::mpsc;

use crate::script::Cmd;

/// Where provider events for scripts go: one queue per running script,
/// keyed by its caller name (`speak.lua`). Create it before the providers,
/// hand [`Mailbox::sink`] to whoever builds them, then give the mailbox to
/// [`crate::Scripts::start`].
#[derive(Clone, Default)]
pub struct Mailbox(Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Cmd>>>>);

impl Mailbox {
    pub fn sink(&self) -> EventSink {
        let senders = self.0.clone();
        Arc::new(move |caller: &str, event: ProviderEvent| {
            if let Some(tx) = senders.lock().unwrap().get(caller) {
                let _ = tx.send(Cmd::Event(event));
            }
        })
    }

    pub(crate) fn insert(&self, caller: &str, tx: mpsc::UnboundedSender<Cmd>) {
        self.0.lock().unwrap().insert(caller.to_string(), tx);
    }

    pub(crate) fn remove(&self, caller: &str) {
        self.0.lock().unwrap().remove(caller);
    }
}
