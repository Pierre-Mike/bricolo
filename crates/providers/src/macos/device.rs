//! What `device` does on macOS without a program: the built-in display's
//! brightness through the private DisplayServices framework, and locking
//! the screen through `SACLockScreenImmediate` in the private login
//! framework. Both are loaded at run time; a macOS without them answers
//! `unavailable`.

use std::ffi::{CStr, c_void};

use crate::device::Native;
use crate::error::CallError;

const DISPLAY_SERVICES: &CStr =
    c"/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices";
const LOGIN: &CStr = c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login";

/// A symbol from a framework, or `None`.
fn symbol(framework: &CStr, name: &CStr) -> Option<*mut c_void> {
    // SAFETY: dlopen and dlsym take valid C strings; a handle that stays
    // open for the life of the process is fine.
    unsafe {
        let handle = libc::dlopen(framework.as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            return None;
        }
        let sym = libc::dlsym(handle, name.as_ptr());
        (!sym.is_null()).then_some(sym)
    }
}

unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
}

pub struct MacNative;

impl Native for MacNative {
    fn brightness(&self) -> Option<Result<u8, CallError>> {
        let Some(f) = symbol(DISPLAY_SERVICES, c"DisplayServicesGetBrightness") else {
            return Some(Err(CallError::Unavailable));
        };
        // SAFETY: the signature DisplayServices has had since macOS 10.12.
        let get: extern "C" fn(u32, *mut f32) -> i32 = unsafe { std::mem::transmute(f) };
        let mut level: f32 = 0.0;
        let status = get(unsafe { CGMainDisplayID() }, &mut level);
        Some(if status == 0 {
            Ok((level * 100.0).round().clamp(0.0, 100.0) as u8)
        } else {
            Err(CallError::Failed(
                "the main display does not report its brightness (an external monitor?)".into(),
            ))
        })
    }

    fn set_brightness(&self, level: u8) -> Option<Result<(), CallError>> {
        let Some(f) = symbol(DISPLAY_SERVICES, c"DisplayServicesSetBrightness") else {
            return Some(Err(CallError::Unavailable));
        };
        // SAFETY: as above.
        let set: extern "C" fn(u32, f32) -> i32 = unsafe { std::mem::transmute(f) };
        let status = set(unsafe { CGMainDisplayID() }, f32::from(level) / 100.0);
        Some(if status == 0 {
            Ok(())
        } else {
            Err(CallError::Failed(
                "the main display's brightness cannot be set (an external monitor?)".into(),
            ))
        })
    }

    fn lock(&self) -> Option<Result<(), CallError>> {
        let Some(f) = symbol(LOGIN, c"SACLockScreenImmediate") else {
            return Some(Err(CallError::Unavailable));
        };
        // SAFETY: takes nothing, returns a status.
        let lock: extern "C" fn() -> i32 = unsafe { std::mem::transmute(f) };
        Some(if lock() == 0 {
            Ok(())
        } else {
            Err(CallError::Failed("macOS refused to lock the screen".into()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_private_symbols() {
        assert!(symbol(DISPLAY_SERVICES, c"DisplayServicesGetBrightness").is_some());
        assert!(symbol(LOGIN, c"SACLockScreenImmediate").is_some());
        assert!(symbol(LOGIN, c"NoSuchSymbol").is_none());
    }
}
