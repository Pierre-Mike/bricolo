//! `mouse`: the pointer of this device. The macOS backend posts CGEvents
//! (`macos/mouse.rs`), the Linux one sends XTEST fake input
//! (`linux/mouse.rs`). Points use `window`'s coordinates.
//!
//! `mouse` never runs on a peer: a node clicking on another device's
//! screen, unseen, is too dangerous to route.

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    /// "left", "right" or "middle", in any case.
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "middle" => Some(Self::Middle),
            _ => None,
        }
    }
}

/// `Failed("click count must be 1 to 3")` unless `count` is 1 to 3.
pub fn check_click_count(count: u8) -> Result<(), CallError> {
    if (1..=3).contains(&count) {
        Ok(())
    } else {
        Err(CallError::Failed("click count must be 1 to 3".into()))
    }
}

#[async_trait]
pub trait MouseProvider: Send + Sync {
    async fn position(&self, caller: &Caller) -> Result<Point, CallError>;
    async fn move_to(&self, caller: &Caller, to: Point) -> Result<(), CallError>;
    async fn click(&self, caller: &Caller, button: MouseButton, count: u8)
    -> Result<(), CallError>;
    async fn scroll(&self, caller: &Caller, dx: i32, dy: i32) -> Result<(), CallError>;
}

/// No mouse on this node.
pub struct NoMouse;

#[async_trait]
impl MouseProvider for NoMouse {
    async fn position(&self, _: &Caller) -> Result<Point, CallError> {
        Err(CallError::Unavailable)
    }

    async fn move_to(&self, _: &Caller, _: Point) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn click(&self, _: &Caller, _: MouseButton, _: u8) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn scroll(&self, _: &Caller, _: i32, _: i32) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_counts_are_one_to_three() {
        assert!(check_click_count(1).is_ok());
        assert!(check_click_count(3).is_ok());
        for bad in [0, 4, 255] {
            assert_eq!(
                check_click_count(bad),
                Err(CallError::Failed("click count must be 1 to 3".into()))
            );
        }
    }

    #[test]
    fn buttons_parse_by_name() {
        assert_eq!(MouseButton::parse("left"), Some(MouseButton::Left));
        assert_eq!(MouseButton::parse("Right"), Some(MouseButton::Right));
        assert_eq!(MouseButton::parse("MIDDLE"), Some(MouseButton::Middle));
        assert_eq!(MouseButton::parse("thumb"), None);
    }
}
