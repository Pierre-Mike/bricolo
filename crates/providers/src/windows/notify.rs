//! `notify` as a Windows toast, through the WinRT `ToastNotificationManager`.
//!
//! Windows shows a toast only for an app it knows by AppUserModelID. A
//! packaged app has one; the node is an unpackaged program, so on the first
//! call it registers [`AUMID`] under
//! `HKCU\Software\Classes\AppUserModelId\<AUMID>` with the display name
//! "bricolo", as the Windows App SDK does for unpackaged apps. No
//! Start menu shortcut and no administrator rights are needed. If Windows
//! still does not know the ID (the key cannot be written, or an older build
//! ignores it), the toast goes out under Windows PowerShell's ID, which
//! every Windows has, and shows "Windows PowerShell" as its sender.
//!
//! Toasts the user turned off, for the app or for the whole system (Focus
//! assist does not count), answer `denied`.

use std::sync::OnceLock;

use ::windows::Data::Xml::Dom::XmlDocument;
use ::windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use ::windows::Win32::Foundation::ERROR_SUCCESS;
use ::windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW,
};
use ::windows::core::{HSTRING, PCWSTR};
use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;
use crate::notify::{NotifyProvider, toast_xml};

/// The node's AppUserModelID.
pub const AUMID: &str = "com.pierre-mike.bricolo";
/// What the toast shows as its sender.
const DISPLAY_NAME: &str = "bricolo";
/// Windows PowerShell's AppUserModelID, from its Start menu shortcut.
const POWERSHELL_AUMID: &str =
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";

pub struct ToastNotify {
    /// The ID that worked, settled on the first call.
    aumid: OnceLock<&'static str>,
}

impl ToastNotify {
    pub fn new() -> Self {
        Self {
            aumid: OnceLock::new(),
        }
    }
}

impl Default for ToastNotify {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes the registry key that makes [`AUMID`] a known sender.
fn register() -> Result<(), String> {
    let path = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{AUMID}"));
    let mut key = HKEY::default();
    // SAFETY: plain registry calls; `key` is closed below.
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &path,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(format!("cannot create {path}: {status:?}"));
        }
        let name: Vec<u8> = DISPLAY_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        let status = RegSetValueExW(
            key,
            &HSTRING::from("DisplayName"),
            None,
            REG_SZ,
            Some(&name),
        );
        let _ = RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return Err(format!("cannot set DisplayName: {status:?}"));
        }
    }
    Ok(())
}

/// A notifier for `aumid` and whether toasts are on for it. `Setting`
/// fails with "Element not found" (0x80070490) when Windows keeps no
/// notification settings for the sender yet, as on a fresh Windows Server;
/// `Show` still works then, so an unreadable setting counts as on.
fn notifier(aumid: &str) -> ::windows::core::Result<(ToastNotifier, NotificationSetting)> {
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?;
    let setting = notifier.Setting().unwrap_or_else(|e| {
        tracing::debug!("notify: no notification setting for {aumid} ({e})");
        NotificationSetting::Enabled
    });
    Ok((notifier, setting))
}

/// Our own ID when Windows accepts it, else PowerShell's.
fn pick() -> &'static str {
    if let Err(e) = register() {
        tracing::debug!("notify: {e}");
    }
    match notifier(AUMID) {
        Ok(_) => AUMID,
        Err(e) => {
            tracing::info!(
                "notify: Windows does not know {AUMID} ({e}); toasts come from PowerShell"
            );
            POWERSHELL_AUMID
        }
    }
}

fn show(aumid: &str, title: &str, body: &str) -> Result<(), CallError> {
    let (notifier, setting) = notifier(aumid).map_err(CallError::failed)?;
    if setting != NotificationSetting::Enabled {
        tracing::debug!("notify: toasts are off for {aumid} ({setting:?})");
        return Err(CallError::Denied);
    }
    let doc = XmlDocument::new().map_err(CallError::failed)?;
    doc.LoadXml(&HSTRING::from(toast_xml(title, body)))
        .map_err(CallError::failed)?;
    let toast = ToastNotification::CreateToastNotification(&doc).map_err(CallError::failed)?;
    notifier.Show(&toast).map_err(CallError::failed)
}

#[async_trait]
impl NotifyProvider for ToastNotify {
    async fn show(&self, _: &Caller, title: String, body: String) -> Result<(), CallError> {
        let aumid = match self.aumid.get() {
            Some(aumid) => *aumid,
            None => {
                let picked = tokio::task::spawn_blocking(pick)
                    .await
                    .map_err(CallError::failed)?;
                *self.aumid.get_or_init(|| picked)
            }
        };
        tokio::task::spawn_blocking(move || show(aumid, &title, &body))
            .await
            .map_err(CallError::failed)?
    }
}
