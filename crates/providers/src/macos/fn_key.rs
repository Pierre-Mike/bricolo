//! The Fn (Globe) key. It is a modifier, so Carbon hotkeys never see it
//! alone; a listen-only event tap on the main run loop watches
//! `flagsChanged` events and reports each change of the Fn flag. The tap
//! needs the Input Monitoring (or Accessibility) permission. It only
//! listens, so macOS still runs what System Settings, Keyboard, "Press 🌐
//! key to" says; set it to "Do Nothing" to keep Fn for bricolo.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CGRequestListenEventAccess,
};

use super::main_loop::PressHandler;

/// The OS id the press handler receives for Fn. Carbon ids are hashes of
/// the chord, and no chord hashes to this one in practice.
pub const OS_ID: u32 = u32::MAX;

/// The new state of Fn, when `flags` changes it from `was`.
pub fn fn_change(was: bool, flags: CGEventFlags) -> Option<bool> {
    let down = flags.contains(CGEventFlags::MaskSecondaryFn);
    (down != was).then_some(down)
}

/// What the callback reads. Boxed, so its address stays put while the tap
/// holds it.
struct State {
    handler: Arc<Mutex<Option<PressHandler>>>,
    down: Cell<bool>,
    port: Cell<Option<NonNull<CFMachPort>>>,
}

/// A running tap. Dropping it removes the tap.
pub struct Tap {
    port: CFRetained<CFMachPort>,
    source: CFRetained<CFRunLoopSource>,
    _state: Box<State>,
}

impl Tap {
    /// Starts the tap on the main run loop. Call it on the main thread.
    pub fn start(handler: Arc<Mutex<Option<PressHandler>>>) -> Result<Self, String> {
        let state = Box::new(State {
            handler,
            down: Cell::new(false),
            port: Cell::new(None),
        });
        let mask = 1u64 << CGEventType::FlagsChanged.0;
        // SAFETY: `callback` matches CGEventTapCallBack, and `state` lives
        // in the returned `Tap`, which removes the tap before freeing it.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                mask,
                Some(callback),
                &*state as *const State as *mut c_void,
            )
        };
        let Some(port) = port else {
            // Lists bricolo under Input Monitoring and asks the user, once.
            CGRequestListenEventAccess();
            return Err("macOS refused the Fn key tap: turn on bricolo node in \
                 System Settings, Privacy & Security, Input Monitoring"
                .into());
        };
        state.port.set(Some(NonNull::from(&*port)));
        let source = CFMachPort::new_run_loop_source(None, Some(&port), 0)
            .ok_or("cannot add the Fn key tap to the run loop")?;
        let main = CFRunLoop::main().ok_or("no main run loop")?;
        // SAFETY: a static CFString that CoreFoundation defines.
        main.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        CGEvent::tap_enable(&port, true);
        Ok(Self {
            port,
            source,
            _state: state,
        })
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        CGEvent::tap_enable(&self.port, false);
        self.source.invalidate();
        self.port.invalidate();
    }
}

unsafe extern "C-unwind" fn callback(
    _: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `info` is the `State` that `Tap::start` passed, still alive.
    let state = unsafe { &*(info as *const State) };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        // macOS turns a slow tap off; turn it back on.
        if let Some(port) = state.port.get() {
            // SAFETY: the port lives as long as the `Tap` that owns `state`.
            CGEvent::tap_enable(unsafe { port.as_ref() }, true);
        }
    } else if kind == CGEventType::FlagsChanged {
        // SAFETY: macOS passes a live event.
        let flags = CGEvent::flags(Some(unsafe { event.as_ref() }));
        if let Some(down) = fn_change(state.down.get(), flags) {
            state.down.set(down);
            let handler = state.handler.lock().unwrap().clone();
            if let Some(handler) = handler {
                handler(OS_ID, down);
            }
        }
    }
    event.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_changes_of_the_fn_flag() {
        let fn_down = CGEventFlags::MaskSecondaryFn;
        assert_eq!(fn_change(false, fn_down), Some(true));
        assert_eq!(fn_change(true, fn_down | CGEventFlags::MaskShift), None);
        assert_eq!(fn_change(true, CGEventFlags::MaskShift), Some(false));
        assert_eq!(fn_change(false, CGEventFlags::MaskCommand), None);
    }
}
