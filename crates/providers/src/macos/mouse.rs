//! `mouse` on macOS: CGEvents posted at the HID event tap, as `keyboard`
//! does. Posting needs the Accessibility permission; without it macOS
//! drops the events silently, so moving, clicking and scrolling check
//! first and answer `denied`. Reading the position needs no permission.

use std::sync::Once;
use std::time::Duration;

use async_trait::async_trait;
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventType, CGMouseButton,
    CGPreflightPostEventAccess, CGRequestPostEventAccess, CGScrollEventUnit,
};

use crate::caller::Caller;
use crate::error::CallError;
use crate::mouse::{self, MouseButton, MouseProvider, Point};

/// A pause between events, so slow apps keep up.
const GAP: Duration = Duration::from_millis(8);

pub struct MacMouse;

fn check_access() -> Result<(), CallError> {
    static ASKED: Once = Once::new();
    if CGPreflightPostEventAccess() {
        return Ok(());
    }
    ASKED.call_once(|| {
        tracing::warn!(
            "mouse: macOS refuses mouse events; turn on bricolo node in \
             System Settings, Privacy & Security, Accessibility (bricolo permissions --request)"
        );
        CGRequestPostEventAccess();
    });
    Err(CallError::Denied)
}

fn cannot() -> CallError {
    CallError::Failed("cannot create a mouse event".into())
}

fn position() -> Result<Point, CallError> {
    let event = CGEvent::new(None).ok_or_else(cannot)?;
    let at = CGEvent::location(Some(&event));
    Ok(Point {
        x: at.x.round() as i32,
        y: at.y.round() as i32,
    })
}

fn cg_point(p: Point) -> CGPoint {
    CGPoint {
        x: f64::from(p.x),
        y: f64::from(p.y),
    }
}

fn move_to(to: Point) -> Result<(), CallError> {
    check_access()?;
    let event = CGEvent::new_mouse_event(
        None,
        CGEventType::MouseMoved,
        cg_point(to),
        CGMouseButton::Left,
    )
    .ok_or_else(cannot)?;
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

/// The down event, the up event and the CGEvent button for `button`.
fn kinds(button: MouseButton) -> (CGEventType, CGEventType, CGMouseButton) {
    match button {
        MouseButton::Left => (
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGMouseButton::Left,
        ),
        MouseButton::Right => (
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGMouseButton::Right,
        ),
        MouseButton::Middle => (
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGMouseButton::Center,
        ),
    }
}

fn click(button: MouseButton, count: u8) -> Result<(), CallError> {
    check_access()?;
    let at = cg_point(position()?);
    let (down, up, cg_button) = kinds(button);
    for n in 1..=i64::from(count) {
        for kind in [down, up] {
            let event = CGEvent::new_mouse_event(None, kind, at, cg_button).ok_or_else(cannot)?;
            CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventClickState, n);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
            std::thread::sleep(GAP);
        }
    }
    Ok(())
}

fn scroll(dx: i32, dy: i32) -> Result<(), CallError> {
    check_access()?;
    // CGEvent's wheel 1 is vertical and positive scrolls up; ours scrolls
    // down. Wheel 2 is horizontal, positive to the left; ours is to the right.
    let event = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Line, 2, -dy, -dx, 0)
        .ok_or_else(cannot)?;
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    Ok(())
}

#[async_trait]
impl MouseProvider for MacMouse {
    async fn position(&self, _: &Caller) -> Result<Point, CallError> {
        tokio::task::spawn_blocking(position)
            .await
            .map_err(CallError::failed)?
    }

    async fn move_to(&self, _: &Caller, to: Point) -> Result<(), CallError> {
        tokio::task::spawn_blocking(move || move_to(to))
            .await
            .map_err(CallError::failed)?
    }

    async fn click(&self, _: &Caller, button: MouseButton, count: u8) -> Result<(), CallError> {
        mouse::check_click_count(count)?;
        tokio::task::spawn_blocking(move || click(button, count))
            .await
            .map_err(CallError::failed)?
    }

    async fn scroll(&self, _: &Caller, dx: i32, dy: i32) -> Result<(), CallError> {
        tokio::task::spawn_blocking(move || scroll(dx, dy))
            .await
            .map_err(CallError::failed)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_map_to_their_events() {
        let (down, up, button) = kinds(MouseButton::Right);
        assert_eq!(down, CGEventType::RightMouseDown);
        assert_eq!(up, CGEventType::RightMouseUp);
        assert_eq!(button, CGMouseButton::Right);
        assert_eq!(kinds(MouseButton::Middle).2, CGMouseButton::Center);
    }
}
