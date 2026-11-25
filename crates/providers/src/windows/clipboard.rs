//! `clipboard` through arboard, which uses the Win32 clipboard
//! (`CF_UNICODETEXT`). Windows copies what we set into the clipboard, so
//! nothing has to stay alive to keep it, and each call opens the clipboard
//! on a blocking thread and closes it again. arboard retries while another
//! program holds the clipboard open.

use async_trait::async_trait;

use crate::caller::Caller;
use crate::clipboard::ClipboardProvider;
use crate::error::CallError;

pub struct WinClipboard;

async fn with_clipboard<T: Send + 'static>(
    f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, CallError> + Send + 'static,
) -> Result<T, CallError> {
    tokio::task::spawn_blocking(move || {
        let mut board = arboard::Clipboard::new().map_err(|e| {
            tracing::debug!("clipboard: cannot open: {e}");
            CallError::Unavailable
        })?;
        f(&mut board)
    })
    .await
    .map_err(CallError::failed)?
}

#[async_trait]
impl ClipboardProvider for WinClipboard {
    async fn get(&self, _: &Caller) -> Result<Option<String>, CallError> {
        with_clipboard(|b| match b.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(CallError::failed(e)),
        })
        .await
    }

    async fn set(&self, _: &Caller, text: String) -> Result<(), CallError> {
        with_clipboard(move |b| b.set_text(text).map_err(CallError::failed)).await
    }
}
