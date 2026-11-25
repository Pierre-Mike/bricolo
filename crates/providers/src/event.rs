use std::sync::Arc;

use crate::ui::UiEvent;

/// Something a provider delivers to one plugin later, outside any call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderEvent {
    Timer(u64),
    Hotkey(String),
    /// The chord bound under this id was released.
    HotkeyReleased(String),
    /// The user acted on the plugin's tile or panel.
    Ui(UiEvent),
    /// An app launched, quit or came to the front (`app.watch`).
    App(AppEvent),
    /// The device slept, woke, locked or unlocked (`system.watch`).
    System(SystemEvent),
    /// Something changed under a folder watched with `files.watch`.
    File(FileEvent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppChange {
    Launched,
    Terminated,
    Activated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppEvent {
    pub change: AppChange,
    pub app: crate::app::AppInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemEvent {
    WillSleep,
    DidWake,
    ScreenLocked,
    ScreenUnlocked,
    ScreensChanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChange {
    Created,
    Modified,
    Removed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEvent {
    pub watch: u64,
    pub path: String,
    pub change: FileChange,
}

/// Delivers an event to the named plugin's queue. The runtime supplies it.
pub type EventSink = Arc<dyn Fn(&str, ProviderEvent) + Send + Sync>;
