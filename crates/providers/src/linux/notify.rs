//! `notify` through the freedesktop notification service on the session
//! bus (`org.freedesktop.Notifications`), which GNOME, KDE, dunst, mako and
//! xfce4-notifyd all serve. No session bus, or nobody serving it, answers
//! `unavailable`.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::caller::Caller;
use crate::error::CallError;
use crate::notify::NotifyProvider;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// A notification server that is being started by D-Bus activation can
/// take a moment; one that hangs longer than this is not coming.
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct DbusNotify {
    /// Opened on the first call, and again after a failure.
    bus: Mutex<Option<zbus::Connection>>,
}

impl DbusNotify {
    pub fn new() -> Self {
        Self {
            bus: Mutex::default(),
        }
    }

    async fn bus(&self) -> Result<zbus::Connection, CallError> {
        let mut slot = self.bus.lock().await;
        if let Some(bus) = &*slot {
            return Ok(bus.clone());
        }
        let bus = zbus::Connection::session().await.map_err(|e| {
            tracing::debug!("notify: no D-Bus session bus: {e}");
            CallError::Unavailable
        })?;
        *slot = Some(bus.clone());
        Ok(bus)
    }
}

impl Default for DbusNotify {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NotifyProvider for DbusNotify {
    async fn show(&self, _: &Caller, title: String, body: String) -> Result<(), CallError> {
        let bus = self.bus().await?;
        // app_name, replaces_id, app_icon, summary, body, actions, hints,
        // expire_timeout (-1: the server's default).
        let args = (
            "bricolo",
            0u32,
            "",
            title.as_str(),
            body.as_str(),
            Vec::<&str>::new(),
            HashMap::<&str, zbus::Variant>::new(),
            -1i32,
        );
        let call = bus.call_method(Some(SERVICE), PATH, Some(SERVICE), "Notify", &args);
        match tokio::time::timeout(TIMEOUT, call).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(zbus::Error::MethodError(name, detail, _)))
                if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
            {
                tracing::debug!("notify: no notification server: {detail:?}");
                Err(CallError::Unavailable)
            }
            Ok(Err(e)) => {
                self.bus.lock().await.take();
                Err(CallError::failed(e))
            }
            Err(_) => Err(CallError::Timeout),
        }
    }
}
