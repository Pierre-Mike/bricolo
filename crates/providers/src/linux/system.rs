//! `system` on Linux.
//!
//! - `power` reads `/sys/class/power_supply` ([`read_power_supply`]).
//! - `idle-seconds` asks the X server's MIT-SCREEN-SAVER extension on X11.
//!   Elsewhere, or without the extension, it asks logind for the session's
//!   `IdleHint` and `IdleSinceHint`, which the desktop sets after its own
//!   idle delay, so it reads 0 until then.
//! - `keep-awake` takes a logind inhibitor lock (`idle:sleep`, mode
//!   `block`) on the system bus and holds its file descriptor per plugin;
//!   closing it ends the lock.
//! - `watch` follows logind's `PrepareForSleep` (will sleep, did wake), the
//!   session's `Lock` and `Unlock` signals and its `LockedHint` (locked,
//!   unlocked), and on X11 RandR's screen change notifications (screens
//!   changed).
//!
//! Without a system bus or logind, as in most containers, the logind parts
//! answer `unavailable` within [`TIMEOUT`].

use std::collections::{HashMap, HashSet};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
use x11rb::protocol::screensaver::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use zbus::message::Type as MessageType;
use zbus::{MatchRule, MessageStream, OwnedObjectPath, OwnedVariant};

use super::{Session, warn_once};
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent, SystemEvent};
use crate::system::{Power, SystemProvider, lock_change, logind_idle_seconds, read_power_supply};

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const SESSION: &str = "org.freedesktop.login1.Session";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// How long a D-Bus call to logind may take.
pub const TIMEOUT: Duration = Duration::from_secs(5);

type Watchers = Arc<Mutex<HashSet<String>>>;

pub struct LinuxSystem {
    sink: EventSink,
    watchers: Watchers,
    power_dir: PathBuf,
    /// A bus address instead of the system bus, for the tests.
    bus_address: Option<String>,
    /// The system bus, opened on first use and again after a failure.
    bus: tokio::sync::Mutex<Option<zbus::Connection>>,
    /// Each plugin's inhibitor lock.
    awake: Mutex<HashMap<String, OwnedFd>>,
    /// Forwards logind's signals while it runs.
    logind: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Whether the RandR thread is listening.
    randr: Arc<AtomicBool>,
    warned: AtomicBool,
}

impl LinuxSystem {
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            watchers: Arc::default(),
            power_dir: PathBuf::from("/sys/class/power_supply"),
            bus_address: None,
            bus: tokio::sync::Mutex::default(),
            awake: Mutex::default(),
            logind: Mutex::default(),
            randr: Arc::default(),
            warned: AtomicBool::new(false),
        }
    }

    async fn bus(&self) -> Result<zbus::Connection, CallError> {
        let mut slot = self.bus.lock().await;
        if let Some(bus) = &*slot {
            return Ok(bus.clone());
        }
        let connect = async {
            match &self.bus_address {
                Some(address) => {
                    zbus::connection::Builder::address(address.as_str())
                        .build()
                        .await
                }
                None => zbus::Connection::system().await,
            }
        };
        let bus = match tokio::time::timeout(TIMEOUT, connect).await {
            Ok(Ok(bus)) => bus,
            Ok(Err(e)) => {
                warn_once(
                    &self.warned,
                    &format!("system: no D-Bus system bus ({e}), so logind is unavailable"),
                );
                return Err(CallError::Unavailable);
            }
            Err(_) => {
                warn_once(&self.warned, "system: the D-Bus system bus did not answer");
                return Err(CallError::Unavailable);
            }
        };
        *slot = Some(bus.clone());
        Ok(bus)
    }

    /// Calls a logind method and returns the reply.
    async fn call<B>(
        &self,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
    ) -> Result<zbus::Message, CallError>
    where
        B: serde::Serialize + zbus::DynamicType,
    {
        let bus = self.bus().await?;
        let call = bus.call_method(Some(LOGIND), path, Some(interface), method, body);
        match tokio::time::timeout(TIMEOUT, call).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(e)) => Err(self.dbus_error(e).await),
            Err(_) => Err(CallError::Timeout),
        }
    }

    async fn dbus_error(&self, e: zbus::Error) -> CallError {
        match &e {
            zbus::Error::MethodError(name, detail, _) => match name.as_str() {
                "org.freedesktop.DBus.Error.ServiceUnknown"
                | "org.freedesktop.DBus.Error.NameHasNoOwner" => {
                    warn_once(&self.warned, "system: logind is not running");
                    CallError::Unavailable
                }
                "org.freedesktop.DBus.Error.AccessDenied"
                | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => {
                    CallError::Denied
                }
                _ => CallError::Failed(format!(
                    "logind: {name}: {}",
                    detail.as_deref().unwrap_or("")
                )),
            },
            _ => {
                // The connection may be broken: open a new one next time.
                self.bus.lock().await.take();
                CallError::failed(e)
            }
        }
    }

    /// The object path of this user's session: the caller's own, or the
    /// user's graphical one when the node runs as a user service.
    async fn session_path(&self) -> Result<OwnedObjectPath, CallError> {
        let reply = self
            .call(MANAGER_PATH, MANAGER, "GetSession", &("auto",))
            .await?;
        reply.body().deserialize().map_err(CallError::failed)
    }

    async fn property(
        &self,
        path: &str,
        interface: &str,
        name: &str,
    ) -> Result<OwnedVariant, CallError> {
        let reply = self
            .call(path, PROPERTIES, "Get", &(interface, name))
            .await?;
        reply.body().deserialize().map_err(CallError::failed)
    }

    async fn logind_idle(&self) -> Result<u64, CallError> {
        let path = self.session_path().await?;
        let idle = self.property(path.as_str(), SESSION, "IdleHint").await?;
        let since = self
            .property(path.as_str(), SESSION, "IdleSinceHint")
            .await?;
        let idle = bool::try_from(idle).map_err(CallError::failed)?;
        let since = u64::try_from(since).map_err(CallError::failed)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or_default();
        Ok(logind_idle_seconds(idle, since, now))
    }

    /// Starts forwarding logind's signals unless it already runs.
    async fn start_logind(&self) -> Result<(), CallError> {
        if self
            .logind
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return Ok(());
        }
        let bus = self.bus().await?;
        let session = self.session_path().await.ok();
        let mut rules = vec![
            MatchRule::builder()
                .msg_type(MessageType::Signal)
                .path(MANAGER_PATH)
                .interface(MANAGER)
                .member("PrepareForSleep")
                .build(),
        ];
        let mut locked = None;
        if let Some(path) = &session {
            rules.push(
                MatchRule::builder()
                    .msg_type(MessageType::Signal)
                    .path(path.as_str())
                    .interface(SESSION)
                    .build(),
            );
            rules.push(
                MatchRule::builder()
                    .msg_type(MessageType::Signal)
                    .path(path.as_str())
                    .interface(PROPERTIES)
                    .member("PropertiesChanged")
                    .arg(0, SESSION)
                    .build(),
            );
            locked = self
                .property(path.as_str(), SESSION, "LockedHint")
                .await
                .ok()
                .and_then(|v| bool::try_from(v).ok());
        }
        let mut streams = Vec::new();
        for rule in rules {
            let rule = rule.map_err(CallError::failed)?;
            let stream = MessageStream::for_match_rule(rule, &bus, None);
            match tokio::time::timeout(TIMEOUT, stream).await {
                Ok(Ok(stream)) => streams.push(stream),
                Ok(Err(e)) => return Err(self.dbus_error(e).await),
                Err(_) => return Err(CallError::Timeout),
            }
        }
        let (sink, watchers) = (self.sink.clone(), self.watchers.clone());
        let task = tokio::spawn(async move {
            let mut messages = futures_util::stream::select_all(streams);
            while let Some(Ok(msg)) = messages.next().await {
                if let Some(event) = logind_event(&msg, &mut locked) {
                    deliver(&sink, &watchers, event);
                }
            }
            tracing::debug!("system: the system bus closed; watch starts again on the next call");
        });
        *self.logind.lock().unwrap() = Some(task);
        Ok(())
    }

    /// Starts the RandR listener on X11 unless it already runs.
    async fn start_randr(&self) -> Result<(), CallError> {
        if Session::current() != Session::X11 {
            return Err(CallError::Unavailable);
        }
        if self.randr.load(Ordering::SeqCst) {
            return Ok(());
        }
        let conn = tokio::task::spawn_blocking(|| -> Result<RustConnection, String> {
            let (conn, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
            let root = conn.setup().roots[screen].root;
            conn.randr_select_input(root, NotifyMask::SCREEN_CHANGE)
                .map_err(|e| e.to_string())?
                .check()
                .map_err(|e| e.to_string())?;
            Ok(conn)
        })
        .await
        .map_err(CallError::failed)?
        .map_err(|e| {
            tracing::debug!("system: no RandR screen changes: {e}");
            CallError::Unavailable
        })?;
        if self.randr.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let (sink, watchers, running) =
            (self.sink.clone(), self.watchers.clone(), self.randr.clone());
        std::thread::Builder::new()
            .name("bricolo-randr".into())
            .spawn(move || {
                loop {
                    match conn.wait_for_event() {
                        Ok(Event::RandrScreenChangeNotify(_)) => {
                            deliver(&sink, &watchers, SystemEvent::ScreensChanged);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::debug!("system: lost the X server: {e}");
                            break;
                        }
                    }
                }
                running.store(false, Ordering::SeqCst);
            })
            .map_err(CallError::failed)?;
        Ok(())
    }
}

/// Sends `event` to every plugin watching.
fn deliver(sink: &EventSink, watchers: &Watchers, event: SystemEvent) {
    let plugins: Vec<String> = watchers.lock().unwrap().iter().cloned().collect();
    for plugin in plugins {
        sink(&plugin, ProviderEvent::System(event));
    }
}

/// The event a logind signal stands for, if any.
fn logind_event(msg: &zbus::Message, locked: &mut Option<bool>) -> Option<SystemEvent> {
    let header = msg.header();
    match header.member()?.as_str() {
        "PrepareForSleep" => {
            let going: bool = msg.body().deserialize().ok()?;
            Some(if going {
                SystemEvent::WillSleep
            } else {
                SystemEvent::DidWake
            })
        }
        "Lock" => lock_change(locked, true),
        "Unlock" => lock_change(locked, false),
        "PropertiesChanged" => {
            let (_, changed, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
                msg.body().deserialize().ok()?;
            let hint = bool::try_from(changed.get("LockedHint")?).ok()?;
            lock_change(locked, hint)
        }
        _ => None,
    }
}

/// Milliseconds since the last input, from the X server's screen saver
/// extension.
fn x11_idle_ms() -> Result<u64, String> {
    let (conn, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = conn.setup().roots[screen].root;
    let info = conn
        .screensaver_query_info(root)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    Ok(info.ms_since_user_input.into())
}

#[async_trait]
impl SystemProvider for LinuxSystem {
    async fn idle_seconds(&self, _: &Caller) -> Result<u64, CallError> {
        if Session::current() == Session::X11 {
            match tokio::task::spawn_blocking(x11_idle_ms).await {
                Ok(Ok(ms)) => return Ok(ms / 1000),
                Ok(Err(e)) => tracing::debug!("system: no X11 idle time ({e}), asking logind"),
                Err(e) => return Err(CallError::failed(e)),
            }
        }
        self.logind_idle().await
    }

    async fn power(&self, _: &Caller) -> Result<Power, CallError> {
        let dir = self.power_dir.clone();
        tokio::task::spawn_blocking(move || read_power_supply(&dir))
            .await
            .map_err(CallError::failed)
    }

    async fn keep_awake(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        if !on {
            self.awake.lock().unwrap().remove(&caller.plugin);
            return Ok(());
        }
        if self.awake.lock().unwrap().contains_key(&caller.plugin) {
            return Ok(());
        }
        let why = format!("{} asked to keep awake", caller.plugin);
        let reply = self
            .call(
                MANAGER_PATH,
                MANAGER,
                "Inhibit",
                &("idle:sleep", "bricolo", why.as_str(), "block"),
            )
            .await?;
        let fd: zbus::OwnedFd = reply.body().deserialize().map_err(CallError::failed)?;
        self.awake
            .lock()
            .unwrap()
            .insert(caller.plugin.clone(), fd.into());
        Ok(())
    }

    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        if !on {
            self.watchers.lock().unwrap().remove(&caller.plugin);
            return Ok(());
        }
        let logind = self.start_logind().await;
        let randr = self.start_randr().await;
        if let (Err(e), Err(_)) = (&logind, &randr) {
            return Err(e.clone());
        }
        if let Err(e) = logind {
            tracing::debug!("system: watching screens only; logind: {e:?}");
        }
        self.watchers.lock().unwrap().insert(caller.plugin.clone());
        Ok(())
    }

    fn release(&self, plugin: &str) {
        self.watchers.lock().unwrap().remove(plugin);
        self.awake.lock().unwrap().remove(plugin);
    }
}

impl Drop for LinuxSystem {
    fn drop(&mut self) {
        if let Some(task) = self.logind.lock().unwrap().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// A node whose system bus is a socket nobody listens on, as in a
    /// container without D-Bus.
    fn without_bus(dir: &std::path::Path) -> LinuxSystem {
        let mut system = LinuxSystem::new(Arc::new(|_: &str, _| {}));
        let socket = dir.join("no-bus");
        system.bus_address = Some(format!("unix:path={}", socket.display()));
        system.power_dir = dir.join("power_supply");
        system
    }

    #[tokio::test]
    async fn without_a_system_bus_logind_is_unavailable_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let system = without_bus(dir.path());
        let c = Caller::new("p");
        let start = Instant::now();
        assert_eq!(
            system.keep_awake(&c, true).await,
            Err(CallError::Unavailable)
        );
        // Turning off what never started is fine.
        assert_eq!(system.keep_awake(&c, false).await, Ok(()));
        if Session::current() != Session::X11 {
            assert_eq!(system.idle_seconds(&c).await, Err(CallError::Unavailable));
            assert_eq!(system.watch(&c, true).await, Err(CallError::Unavailable));
            assert!(system.watchers.lock().unwrap().is_empty());
        }
        assert_eq!(system.watch(&c, false).await, Ok(()));
        system.release("p");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }

    #[tokio::test]
    async fn power_reads_the_supply_folder() {
        let dir = tempfile::tempdir().unwrap();
        let system = without_bus(dir.path());
        let bat = dir.path().join("power_supply/BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("capacity"), "42\n").unwrap();
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        assert_eq!(
            system.power(&Caller::new("p")).await,
            Ok(Power {
                battery: Some(42),
                plugged_in: false
            })
        );
    }
}
