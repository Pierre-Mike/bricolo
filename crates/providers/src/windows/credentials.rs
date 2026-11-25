//! `secrets` backend: generic credentials in the Windows Credential Manager
//! of the user the node runs as. The target name is
//! "bricolo/<account>" ([`credential_target`]), the user name the
//! account, and the value the UTF-8 bytes of the secret. They persist on
//! this machine (`CRED_PERSIST_LOCAL_MACHINE`), not in a roaming profile.
//! `cmdkey /list:bricolo/*` lists them.

use ::windows::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME};
use ::windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use ::windows::core::{HSTRING, PWSTR};

use crate::error::CallError;
use crate::secrets::{Store, credential_target};

pub struct CredentialStore;

/// UTF-16 with a trailing NUL, for the `PWSTR` fields of `CREDENTIALW`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn not_found(e: &::windows::core::Error) -> bool {
    e.code() == ERROR_NOT_FOUND.to_hresult()
}

impl Store for CredentialStore {
    fn get(&self, account: &str) -> Result<Option<String>, CallError> {
        let target = HSTRING::from(credential_target(account));
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: CredReadW fills `cred` with a buffer we free below.
        match unsafe { CredReadW(&target, CRED_TYPE_GENERIC, None, &mut cred) } {
            Ok(()) => {}
            Err(e) if not_found(&e) => return Ok(None),
            Err(e) => return Err(CallError::failed(e)),
        }
        // SAFETY: on success `cred` points at a valid CREDENTIALW whose blob
        // is `CredentialBlobSize` bytes long; it stays valid until CredFree.
        let bytes = unsafe {
            let c = &*cred;
            let bytes = if c.CredentialBlob.is_null() || c.CredentialBlobSize == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
            };
            CredFree(cred.cast());
            bytes
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| CallError::Failed("secret is not text".into()))
    }

    fn set(&self, account: &str, value: &str) -> Result<(), CallError> {
        if value.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            return Err(CallError::Failed(format!(
                "secret too long: the Credential Manager keeps at most {CRED_MAX_CREDENTIAL_BLOB_SIZE} bytes"
            )));
        }
        let mut target = wide(&credential_target(account));
        let mut user = wide(account);
        let mut blob = value.as_bytes().to_vec();
        let cred = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR::null(),
            LastWritten: FILETIME::default(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: if blob.is_empty() {
                std::ptr::null_mut()
            } else {
                blob.as_mut_ptr()
            },
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: PWSTR::null(),
            UserName: PWSTR(user.as_mut_ptr()),
        };
        // SAFETY: every pointer in `cred` outlives the call; CredWriteW
        // copies what it keeps.
        unsafe { CredWriteW(&cred, 0) }.map_err(CallError::failed)
    }

    fn delete(&self, account: &str) -> Result<(), CallError> {
        let target = HSTRING::from(credential_target(account));
        // SAFETY: a plain call with a NUL-terminated target name.
        match unsafe { CredDeleteW(&target, CRED_TYPE_GENERIC, None) } {
            Ok(()) => Ok(()),
            Err(e) if not_found(&e) => Ok(()),
            Err(e) => Err(CallError::failed(e)),
        }
    }
}
