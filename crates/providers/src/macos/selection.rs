//! `selection` through Accessibility: the system-wide element's focused
//! element, then its `AXSelectedText`.

use async_trait::async_trait;
use objc2_application_services::AXUIElement;

use super::window::{AxErr, element_attr, string_attr};
use crate::caller::Caller;
use crate::error::CallError;
use crate::selection::SelectionProvider;

pub struct MacSelection;

fn read() -> Result<Option<String>, AxErr> {
    let system = unsafe { AXUIElement::new_system_wide() };
    let Some(focused) = element_attr(&system, "AXFocusedUIElement")? else {
        return Ok(None);
    };
    Ok(string_attr(&focused, "AXSelectedText")?.filter(|s| !s.is_empty()))
}

#[async_trait]
impl SelectionProvider for MacSelection {
    async fn get_text(&self, _: &Caller) -> Result<Option<String>, CallError> {
        tokio::task::spawn_blocking(read)
            .await
            .map_err(CallError::failed)?
            .map_err(CallError::from)
    }
}
