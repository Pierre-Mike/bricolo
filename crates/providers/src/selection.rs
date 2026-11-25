//! `selection`: the text selected in the frontmost app. macOS reads it
//! through Accessibility (`macos/selection.rs`), Linux from the PRIMARY
//! selection (`linux/clipboard.rs`). Never runs on a peer.

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

#[async_trait]
pub trait SelectionProvider: Send + Sync {
    async fn get_text(&self, caller: &Caller) -> Result<Option<String>, CallError>;
}

/// No selection on this node.
pub struct NoSelection;

#[async_trait]
impl SelectionProvider for NoSelection {
    async fn get_text(&self, _: &Caller) -> Result<Option<String>, CallError> {
        Err(CallError::Unavailable)
    }
}
