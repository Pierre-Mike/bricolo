//! `notify` through `osascript`. The title and body go in as script
//! arguments, never into the script text, so they cannot inject AppleScript.

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;
use crate::notify::NotifyProvider;

const SCRIPT: &str =
    "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv)\nend run";

pub struct Osascript;

#[async_trait]
impl NotifyProvider for Osascript {
    async fn show(&self, _: &Caller, title: String, body: String) -> Result<(), CallError> {
        let out = tokio::process::Command::new("/usr/bin/osascript")
            .args(["-e", SCRIPT, &title, &body])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(CallError::failed)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(CallError::Failed(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ))
        }
    }
}
