//! `clipboard` on the general NSPasteboard, plain text only.

use async_trait::async_trait;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

use crate::caller::Caller;
use crate::clipboard::ClipboardProvider;
use crate::error::CallError;

pub struct Pasteboard;

fn get() -> Option<String> {
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        // SAFETY: a constant NSString exported by AppKit.
        let kind = unsafe { NSPasteboardTypeString };
        board.stringForType(kind).map(|s| s.to_string())
    })
}

fn set(text: &str) -> Result<(), CallError> {
    autoreleasepool(|_| {
        let board = NSPasteboard::generalPasteboard();
        // SAFETY: as in `get`.
        let kind = unsafe { NSPasteboardTypeString };
        board.clearContents();
        if board.setString_forType(&NSString::from_str(text), kind) {
            Ok(())
        } else {
            Err(CallError::Failed("the pasteboard refused the text".into()))
        }
    })
}

#[async_trait]
impl ClipboardProvider for Pasteboard {
    async fn get(&self, _: &Caller) -> Result<Option<String>, CallError> {
        tokio::task::spawn_blocking(get)
            .await
            .map_err(CallError::failed)
    }

    async fn set(&self, _: &Caller, text: String) -> Result<(), CallError> {
        tokio::task::spawn_blocking(move || set(&text))
            .await
            .map_err(CallError::failed)?
    }
}
