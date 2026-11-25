//! macOS backends: hotkeys and screens on the main-thread loop, windows
//! through the Accessibility API, apps through NSWorkspace and `open`, the
//! clipboard through NSPasteboard, notifications through `osascript`,
//! sounds through AVAudioPlayer.

pub mod accessibility;
pub mod app;
pub mod audio;
pub mod clipboard;
pub mod device;
pub mod events;
pub mod fn_key;
pub mod hotkey;
pub mod keyboard;
pub mod keychain;
pub mod main_loop;
pub mod mouse;
pub mod notify;
pub mod screen;
pub mod selection;
pub mod system;
pub mod window;

use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureDevice, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
};
use objc2_core_graphics::{
    CGPreflightListenEventAccess, CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess,
    CGRequestListenEventAccess, CGRequestPostEventAccess, CGRequestScreenCaptureAccess,
};

use crate::permissions::Permissions;
use crate::window::Rect;

/// Whether this process may use the Accessibility API. When it runs from a
/// terminal, macOS checks the terminal app.
pub fn accessibility_granted() -> bool {
    unsafe { objc2_application_services::AXIsProcessTrusted() }
}

pub fn permissions() -> Permissions {
    Permissions {
        accessibility: Some(accessibility_granted()),
        post_events: Some(CGPreflightPostEventAccess()),
        input_monitoring: Some(CGPreflightListenEventAccess()),
        microphone: Some(capture_status(Capture::Audio) == AVAuthorizationStatus::Authorized),
        camera: Some(capture_status(Capture::Video) == AVAuthorizationStatus::Authorized),
        screen_recording: Some(CGPreflightScreenCaptureAccess()),
    }
}

/// The grants macOS has never asked about. Only Microphone and Camera can
/// tell "never asked" from "denied".
pub fn not_asked() -> Vec<&'static str> {
    let mut out = Vec::new();
    if capture_status(Capture::Audio) == AVAuthorizationStatus::NotDetermined {
        out.push("microphone");
    }
    if capture_status(Capture::Video) == AVAuthorizationStatus::NotDetermined {
        out.push("camera");
    }
    out
}

/// Shows the Accessibility and Input Monitoring prompts for what is
/// missing. macOS shows each at most once per program; later calls only
/// make sure the entries are listed.
pub fn request_permissions() {
    if !CGPreflightPostEventAccess() {
        CGRequestPostEventAccess();
    }
    if !CGPreflightListenEventAccess() {
        CGRequestListenEventAccess();
    }
}

/// Shows the prompt for one grant, named as in `Permissions`. Mic and
/// camera answer later, on a queue of their own; the hub checks again.
pub fn request_grant(grant: &str) {
    match grant {
        // Posting events sits under Accessibility, so its request lists
        // the program there.
        "accessibility" | "post_events" => {
            CGRequestPostEventAccess();
        }
        "input_monitoring" => {
            CGRequestListenEventAccess();
        }
        "screen_recording" => {
            CGRequestScreenCaptureAccess();
        }
        "microphone" => request_capture(Capture::Audio),
        "camera" => request_capture(Capture::Video),
        _ => {}
    }
}

#[derive(Clone, Copy)]
enum Capture {
    Audio,
    Video,
}

fn media_type(capture: Capture) -> Option<&'static AVMediaType> {
    // SAFETY: AVFoundation's constant strings, linked with the framework.
    unsafe {
        match capture {
            Capture::Audio => AVMediaTypeAudio,
            Capture::Video => AVMediaTypeVideo,
        }
    }
}

fn capture_status(capture: Capture) -> AVAuthorizationStatus {
    let Some(media) = media_type(capture) else {
        return AVAuthorizationStatus::Denied;
    };
    // SAFETY: `media` is AVMediaTypeAudio or AVMediaTypeVideo, the only
    // two values this method accepts.
    unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) }
}

fn request_capture(capture: Capture) {
    let Some(media) = media_type(capture) else {
        return;
    };
    let done = block2::RcBlock::new(|_granted: objc2::runtime::Bool| {});
    // SAFETY: as in `capture_status`; AVFoundation copies the block.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &done) };
}

/// Converts an AppKit rect (origin at the bottom-left of the main display,
/// y growing upward) into top-left coordinates.
pub fn flip_rect(x: f64, y: f64, width: f64, height: f64, main_height: f64) -> Rect {
    Rect {
        x: x.round() as i32,
        y: (main_height - (y + height)).round() as i32,
        width: width.max(0.0).round() as u32,
        height: height.max(0.0).round() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_screen_flips_to_origin() {
        assert_eq!(
            flip_rect(0.0, 0.0, 1512.0, 982.0, 982.0),
            Rect {
                x: 0,
                y: 0,
                width: 1512,
                height: 982
            }
        );
    }

    #[test]
    fn visible_frame_starts_below_the_menu_bar() {
        // Menu bar 33 pt tall, Dock 80 pt at the bottom.
        let r = flip_rect(0.0, 80.0, 1512.0, 869.0, 982.0);
        assert_eq!(
            r,
            Rect {
                x: 0,
                y: 33,
                width: 1512,
                height: 869
            }
        );
    }

    #[test]
    fn screen_above_the_main_one_gets_negative_y() {
        let r = flip_rect(0.0, 982.0, 1920.0, 1080.0, 982.0);
        assert_eq!(r.y, -1080);
    }
}
