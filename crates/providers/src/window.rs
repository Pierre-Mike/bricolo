//! `window`: windows and screens. Coordinates are points measured from the
//! top-left corner of the main display, y growing downward. The macOS
//! backend (`macos/window.rs`) uses the Accessibility API.

use async_trait::async_trait;
use serde::Serialize;

use crate::caller::Caller;
use crate::error::CallError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowInfo {
    pub id: u64,
    pub app: String,
    pub title: String,
    pub frame: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Screen {
    pub id: u32,
    pub frame: Rect,
    /// The frame without the menu bar and the Dock.
    pub visible_frame: Rect,
    /// The screen with the menu bar.
    pub main: bool,
}

#[async_trait]
pub trait WindowProvider: Send + Sync {
    /// Windows of the apps that show in the Dock.
    async fn list(&self, caller: &Caller) -> Result<Vec<WindowInfo>, CallError>;
    async fn focused(&self, caller: &Caller) -> Result<Option<WindowInfo>, CallError>;
    /// Raises the window and activates its app.
    async fn focus(&self, caller: &Caller, id: u64) -> Result<(), CallError>;
    async fn set_frame(&self, caller: &Caller, id: u64, frame: Rect) -> Result<(), CallError>;
    /// Closes the window, as its close button does.
    async fn close(&self, caller: &Caller, id: u64) -> Result<(), CallError>;
    /// Minimizes the window, or restores it when `minimized` is false.
    async fn set_minimized(
        &self,
        caller: &Caller,
        id: u64,
        minimized: bool,
    ) -> Result<(), CallError>;
    async fn screens(&self, caller: &Caller) -> Result<Vec<Screen>, CallError>;
}

/// No window control on this node.
pub struct NoWindows;

#[async_trait]
impl WindowProvider for NoWindows {
    async fn list(&self, _: &Caller) -> Result<Vec<WindowInfo>, CallError> {
        Err(CallError::Unavailable)
    }

    async fn focused(&self, _: &Caller) -> Result<Option<WindowInfo>, CallError> {
        Err(CallError::Unavailable)
    }

    async fn focus(&self, _: &Caller, _: u64) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn set_frame(&self, _: &Caller, _: u64, _: Rect) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn close(&self, _: &Caller, _: u64) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn set_minimized(&self, _: &Caller, _: u64, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn screens(&self, _: &Caller) -> Result<Vec<Screen>, CallError> {
        Err(CallError::Unavailable)
    }
}
