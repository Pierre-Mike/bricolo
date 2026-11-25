//! The control pipe's security on Windows: a DACL that grants this user,
//! and nobody else, access. Without it a named pipe gives every local user
//! read access.

use std::ffi::c_void;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// A SECURITY_ATTRIBUTES whose descriptor allows only the current user.
pub struct OwnerOnly {
    attributes: Box<SECURITY_ATTRIBUTES>,
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is allocated once, never written after `new`, and
// freed only in `drop`; Windows only reads it.
unsafe impl Send for OwnerOnly {}
unsafe impl Sync for OwnerOnly {}

impl OwnerOnly {
    pub fn new() -> Result<Self> {
        let sddl = sddl(&current_user_sid()?);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: a valid SDDL string in, a LocalAlloc'd descriptor out,
        // freed in `drop`.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl.as_str()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .with_context(|| format!("bad security descriptor {sddl}"))?;
        let attributes = Box::new(SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        });
        Ok(Self {
            attributes,
            descriptor,
        })
    }

    /// For `ServerOptions::create_with_security_attributes_raw`.
    pub fn as_ptr(&self) -> *mut c_void {
        std::ptr::from_ref::<SECURITY_ATTRIBUTES>(&self.attributes)
            .cast_mut()
            .cast()
    }
}

impl Drop for OwnerOnly {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(Some(HLOCAL(self.descriptor.0))) };
    }
}

/// Full access for `sid`, protected from inherited entries.
fn sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

/// The SID of the user this process runs as, like `S-1-5-21-...`.
fn current_user_sid() -> Result<String> {
    // SAFETY: standard token query; every handle and buffer is released
    // before returning.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .context("cannot open this process's token")?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        // u64s, so TOKEN_USER's pointer is aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let read = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        read.context("cannot read the token's user")?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).context("cannot format the user's SID")?;
        let sid = text.to_string();
        LocalFree(Some(HLOCAL(text.0.cast())));
        Ok(sid?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_is_built_for_this_user() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert_eq!(sddl("S-1-5-21-1"), "D:P(A;;GA;;;S-1-5-21-1)");
        let security = OwnerOnly::new().unwrap();
        assert!(!security.as_ptr().is_null());
    }
}
