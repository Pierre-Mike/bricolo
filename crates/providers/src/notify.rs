//! `notify`: shows a notification. The macOS backend (`macos/notify.rs`)
//! goes through `osascript`, the Linux one (`linux/notify.rs`) through
//! D-Bus, and the Windows one (`windows/notify.rs`) shows a toast built by
//! [`toast_xml`].

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

#[async_trait]
pub trait NotifyProvider: Send + Sync {
    async fn show(&self, caller: &Caller, title: String, body: String) -> Result<(), CallError>;
}

/// No notifications on this node.
pub struct NoNotify;

#[async_trait]
impl NotifyProvider for NoNotify {
    async fn show(&self, _: &Caller, _: String, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// The toast XML for a Windows notification: the `ToastGeneric` template
/// with the title and the body as text, escaped.
pub fn toast_xml(title: &str, body: &str) -> String {
    let mut lines = format!("<text>{}</text>", xml_escape(title));
    if !body.is_empty() {
        lines.push_str(&format!("<text>{}</text>", xml_escape(body)));
    }
    format!("<toast><visual><binding template=\"ToastGeneric\">{lines}</binding></visual></toast>")
}

/// Escapes the five XML special characters, and drops the control
/// characters XML 1.0 forbids.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_xml_escapes_the_text() {
        assert_eq!(
            toast_xml("Build <done>", "a & b \"c\" 'd'\u{7}"),
            "<toast><visual><binding template=\"ToastGeneric\">\
             <text>Build &lt;done&gt;</text>\
             <text>a &amp; b &quot;c&quot; &apos;d&apos;</text>\
             </binding></visual></toast>"
        );
        assert_eq!(
            toast_xml("Hi", ""),
            "<toast><visual><binding template=\"ToastGeneric\"><text>Hi</text></binding></visual></toast>"
        );
    }
}
