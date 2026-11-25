//! `app`: launch apps, open URLs, and see what runs. The macOS backend
//! (`macos/app.rs`) uses `open` and NSWorkspace; the Linux one
//! (`linux/app.rs`) desktop files, `xdg-open` and the X11 window list.

use async_trait::async_trait;
use serde::Serialize;

use crate::caller::Caller;
use crate::error::CallError;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
}

#[async_trait]
pub trait AppProvider: Send + Sync {
    /// Launches the app, or brings it to the front if it runs. `app` is a
    /// bundle id ("com.apple.Safari") or a name ("Safari").
    async fn launch(&self, caller: &Caller, app: String) -> Result<(), CallError>;
    /// Opens `url` with its default app. `file:` URLs are refused.
    async fn open_url(&self, caller: &Caller, url: String) -> Result<(), CallError>;
    /// Apps that show in the Dock.
    async fn running(&self, caller: &Caller) -> Result<Vec<AppInfo>, CallError>;
    async fn frontmost(&self, caller: &Caller) -> Result<Option<AppInfo>, CallError>;
    /// Starts or stops `app` events for the caller.
    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    /// Drops `plugin`'s watch.
    fn release(&self, _plugin: &str) {}
    /// Asks the app to quit, as Cmd+Q does. `Failed` when no such app runs.
    async fn quit(&self, caller: &Caller, app: String) -> Result<(), CallError>;
    /// Hides the app's windows, or shows them again when `hidden` is false.
    /// `Failed` when no such app runs.
    async fn set_hidden(&self, caller: &Caller, app: String, hidden: bool)
    -> Result<(), CallError>;
}

/// No app control on this node.
pub struct NoApps;

#[async_trait]
impl AppProvider for NoApps {
    async fn launch(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn open_url(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn running(&self, _: &Caller) -> Result<Vec<AppInfo>, CallError> {
        Err(CallError::Unavailable)
    }

    async fn frontmost(&self, _: &Caller) -> Result<Option<AppInfo>, CallError> {
        Err(CallError::Unavailable)
    }

    async fn watch(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn quit(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn set_hidden(&self, _: &Caller, _: String, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// The `open` arguments for `app`: `-b <id>` for a bundle id (dotted, no
/// spaces), `-a <name>` otherwise. Paths are refused, so `launch` only
/// starts installed apps by name.
pub fn launch_args(app: &str) -> Result<Vec<String>, CallError> {
    let app = check_app(app)?;
    let bundle = app.contains('.')
        && !app.contains(' ')
        && !app.ends_with(".app")
        && app
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b));
    let flag = if bundle { "-b" } else { "-a" };
    Ok(vec![flag.into(), app.into()])
}

/// `app` trimmed, if it names an app rather than a path or an option.
pub fn check_app(app: &str) -> Result<&str, CallError> {
    let app = app.trim();
    if app.is_empty() || app.contains('/') || app.starts_with('-') {
        return Err(CallError::Failed(format!(
            "{app:?} is not an app name or bundle id"
        )));
    }
    Ok(app)
}

/// A URL with a scheme, not `file:`: opening a local file could run it.
pub fn check_url(url: &str) -> Result<(), CallError> {
    let scheme = url.split_once(':').map(|(s, _)| s).unwrap_or("");
    let valid = !scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
    if !valid {
        return Err(CallError::Failed(format!("{url:?} has no URL scheme")));
    }
    if scheme.eq_ignore_ascii_case("file") {
        return Err(CallError::Denied);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_ids_and_names_pick_the_open_flag() {
        assert_eq!(
            launch_args("com.apple.Safari").unwrap(),
            ["-b", "com.apple.Safari"]
        );
        assert_eq!(launch_args("Safari").unwrap(), ["-a", "Safari"]);
        assert_eq!(launch_args("Visual Studio Code").unwrap()[0], "-a");
        assert_eq!(launch_args("Foo.app").unwrap()[0], "-a");
        assert!(launch_args("/bin/sh").is_err());
        assert!(launch_args("-n").is_err());
        assert!(launch_args(" ").is_err());
    }

    #[test]
    fn urls_need_a_scheme_and_file_is_denied() {
        assert!(check_url("https://example.com").is_ok());
        assert!(check_url("mailto:me@example.com").is_ok());
        assert!(check_url("x-apple.systempreferences:com.apple.preference").is_ok());
        assert_eq!(check_url("file:///etc/hosts"), Err(CallError::Denied));
        assert_eq!(check_url("FILE:///x"), Err(CallError::Denied));
        assert!(check_url("example.com").is_err());
        assert!(check_url("/etc/hosts").is_err());
    }
}
