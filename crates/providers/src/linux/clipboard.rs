//! `clipboard` through arboard: X11 selections, or a Wayland data-control
//! protocol (Sway, Hyprland, KDE Plasma), else XWayland's X11 clipboard.
//! arboard wants one thread per clipboard, so one thread owns it and the
//! calls queue up for it. On X11 the clipboard holds what we set only while
//! the owner lives, so the thread keeps it for the node's lifetime.

use std::sync::Mutex;
use std::sync::mpsc;

use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::caller::Caller;
use crate::clipboard::ClipboardProvider;
use crate::error::CallError;

enum Request {
    Get(oneshot::Sender<Result<Option<String>, CallError>>),
    Primary(oneshot::Sender<Result<Option<String>, CallError>>),
    Set(String, oneshot::Sender<Result<(), CallError>>),
}

pub struct ArboardClipboard {
    /// Started on the first call, so a node that never uses the clipboard
    /// opens no display connection for it.
    requests: Mutex<Option<mpsc::Sender<Request>>>,
}

impl ArboardClipboard {
    pub fn new() -> Self {
        Self {
            requests: Mutex::default(),
        }
    }

    async fn ask<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, CallError>>) -> Request,
    ) -> Result<T, CallError> {
        let (tx, rx) = oneshot::channel();
        let request = make(tx);
        {
            let mut slot = self.requests.lock().unwrap();
            let sender = slot.get_or_insert_with(start);
            if let Err(mpsc::SendError(request)) = sender.send(request) {
                // The thread died: start a new one.
                let fresh = start();
                fresh.send(request).map_err(|_| CallError::Unavailable)?;
                *slot = Some(fresh);
            }
        }
        rx.await.map_err(|_| CallError::Unavailable)?
    }
}

impl Default for ArboardClipboard {
    fn default() -> Self {
        Self::new()
    }
}

fn start() -> mpsc::Sender<Request> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("bricolo-clipboard".into())
        .spawn(move || serve(rx));
    if let Err(e) = spawned {
        tracing::warn!("clipboard: cannot start its thread: {e}");
    }
    tx
}

fn serve(requests: mpsc::Receiver<Request>) {
    let mut board: Option<arboard::Clipboard> = None;
    for request in requests {
        // Open the clipboard on demand, and again after a failure.
        let opened = match &mut board {
            Some(b) => Ok(b),
            None => arboard::Clipboard::new()
                .map(|b| board.insert(b))
                .map_err(|e| {
                    tracing::debug!("clipboard: cannot open: {e}");
                    CallError::Unavailable
                }),
        };
        match request {
            Request::Get(reply) => {
                let result = opened.and_then(|b| match b.get_text() {
                    Ok(text) => Ok(Some(text)),
                    Err(arboard::Error::ContentNotAvailable) => Ok(None),
                    Err(e) => Err(CallError::failed(e)),
                });
                if matches!(result, Err(CallError::Failed(_))) {
                    board = None;
                }
                let _ = reply.send(result);
            }
            Request::Primary(reply) => {
                use arboard::{GetExtLinux, LinuxClipboardKind};
                let result = opened.and_then(|b| {
                    match b.get().clipboard(LinuxClipboardKind::Primary).text() {
                        Ok(text) => Ok(Some(text).filter(|t| !t.is_empty())),
                        Err(arboard::Error::ContentNotAvailable) => Ok(None),
                        Err(e) => Err(CallError::failed(e)),
                    }
                });
                if matches!(result, Err(CallError::Failed(_))) {
                    board = None;
                }
                let _ = reply.send(result);
            }
            Request::Set(text, reply) => {
                let result = opened.and_then(|b| b.set_text(text).map_err(CallError::failed));
                if result.is_err() {
                    board = None;
                }
                let _ = reply.send(result);
            }
        }
    }
}

#[async_trait]
impl ClipboardProvider for ArboardClipboard {
    async fn get(&self, _: &Caller) -> Result<Option<String>, CallError> {
        self.ask(Request::Get).await
    }

    async fn set(&self, _: &Caller, text: String) -> Result<(), CallError> {
        self.ask(|tx| Request::Set(text, tx)).await
    }
}

/// `selection` on Linux: the PRIMARY selection, read on the clipboard's
/// thread.
pub struct PrimarySelection(pub std::sync::Arc<ArboardClipboard>);

#[async_trait]
impl crate::selection::SelectionProvider for PrimarySelection {
    async fn get_text(&self, _: &Caller) -> Result<Option<String>, CallError> {
        self.0.ask(Request::Primary).await
    }
}
