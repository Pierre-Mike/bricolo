//! `hotkey` on Hyprland. Each chord becomes a Hyprland bind whose action
//! emits `custom>>bricolo-hotkey:<token>` on the event socket, and a thread
//! named `bricolo-hypr-events` turns those lines into hotkey events.
//!
//! - A chord Hyprland's config already binds (by key name or by keycode,
//!   as Omarchy's `SUPER + code:10` workspace keys are) is refused, as an
//!   X11 grab another program holds is.
//! - A config reload drops runtime binds, so the thread binds every chord
//!   again on `configreloaded>>`.
//! - Tokens are `<pid>-<n>`, and Lua binds carry the description
//!   `bricolo[<pid>]: <plugin>/<id>`. When the listener starts, it
//!   removes binds a node that is no longer running left behind.

use std::io::BufRead;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{Bind, Dialect, Hyprland, Keys, cmd, mod_names};
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent};
use crate::hotkey::Chord;

const DESCRIPTION: &str = "bricolo";
const EVENT: &str = "bricolo-hotkey:";

#[derive(Clone)]
struct Binding {
    plugin: String,
    id: String,
    chord: String,
    keys: Keys,
    token: String,
}

impl Binding {
    fn description(&self) -> String {
        format!(
            "{DESCRIPTION}[{}]: {}/{}",
            std::process::id(),
            self.plugin,
            self.id
        )
    }
}

#[derive(Default)]
struct Inner {
    table: Mutex<Vec<Binding>>,
    /// Whether the event thread is reading. False before the first bind
    /// and after Hyprland goes away.
    listening: AtomicBool,
    next: AtomicU64,
}

pub struct HyprHotkeys {
    sink: EventSink,
    inner: Arc<Inner>,
    /// Serializes binds, so a check for conflicts and the bind that follows
    /// are not split by another bind.
    lock: tokio::sync::Mutex<()>,
}

impl HyprHotkeys {
    pub fn new(sink: EventSink) -> Self {
        Self {
            sink,
            inner: Arc::default(),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Whether `plugin` holds `id` here, so unbind and release land on the
    /// right backend.
    pub fn holds(&self, plugin: &str, id: &str) -> bool {
        self.inner
            .table
            .lock()
            .unwrap()
            .iter()
            .any(|b| b.plugin == plugin && b.id == id)
    }

    pub async fn bind(
        &self,
        hypr: Hyprland,
        caller: &Caller,
        id: String,
        chord: String,
    ) -> Result<(), CallError> {
        let parsed = Chord::parse(&chord).map_err(CallError::Failed)?;
        let keys = Keys::from_chord(&parsed)
            .ok_or_else(|| CallError::Failed(format!("no Hyprland key name for {chord}")))?;
        let _guard = self.lock.lock().await;
        let owner = self
            .inner
            .table
            .lock()
            .unwrap()
            .iter()
            .find(|b| b.keys == keys)
            .map(|b| (b.plugin.clone(), b.id.clone()));
        match owner {
            Some((plugin, bound)) if plugin == caller.plugin && bound == id => return Ok(()),
            Some((plugin, bound)) => {
                return Err(CallError::Failed(format!(
                    "{chord} is already bound by {plugin} as {bound}"
                )));
            }
            None => {}
        }
        // Rebinding an id to a new chord drops the old chord.
        self.unbind(hypr.clone(), &caller.plugin, &id).await;
        self.listen(&hypr);
        let binding = Binding {
            plugin: caller.plugin.clone(),
            id,
            chord: chord.clone(),
            keys,
            token: format!(
                "{}-{}",
                std::process::id(),
                self.inner.next.fetch_add(1, Ordering::Relaxed)
            ),
        };
        let b = binding.clone();
        super::window::blocking(hypr, move |h| {
            let binds = h.binds()?;
            if binds
                .iter()
                .any(|bind| keys.matches(bind) && !is_ours(bind))
            {
                return Ok(Err(CallError::Failed(format!(
                    "cannot bind {chord}: Hyprland's config already binds it"
                ))));
            }
            if binds.iter().any(|bind| keys.matches(bind)) {
                // Left by a node that stopped: ours by name, not in the table.
                h.run(&cmd::unbind(h.dialect(), &keys))?;
            }
            h.run(&cmd::bind(h.dialect(), &keys, &b.token, &b.description()))?;
            Ok(Ok(()))
        })
        .await??;
        self.inner.table.lock().unwrap().push(binding);
        Ok(())
    }

    pub async fn unbind(&self, hypr: Hyprland, plugin: &str, id: &str) {
        let removed = {
            let mut table = self.inner.table.lock().unwrap();
            let at = table.iter().position(|b| b.plugin == plugin && b.id == id);
            at.map(|i| table.remove(i))
        };
        if let Some(b) = removed {
            let _ =
                super::window::blocking(hypr, move |h| h.run(&cmd::unbind(h.dialect(), &b.keys)))
                    .await
                    .inspect_err(|e| tracing::debug!("hotkey: unbind {}: {e}", b.chord));
        }
    }

    /// Forgets one binding without telling Hyprland, which is gone.
    pub fn release_one(&self, plugin: &str, id: &str) {
        self.inner
            .table
            .lock()
            .unwrap()
            .retain(|b| !(b.plugin == plugin && b.id == id));
    }

    pub fn release(&self, plugin: &str) {
        let dropped: Vec<Binding> = {
            let mut table = self.inner.table.lock().unwrap();
            let (dropped, kept) = table.drain(..).partition(|b| b.plugin == plugin);
            *table = kept;
            dropped
        };
        if dropped.is_empty() {
            return;
        }
        let Some(h) = Hyprland::current() else { return };
        // Called outside async code too: a few local socket writes.
        for b in dropped {
            if let Err(e) = h.run(&cmd::unbind(h.dialect(), &b.keys)) {
                tracing::debug!("hotkey: unbind {}: {e}", b.chord);
            }
        }
    }

    pub fn bound(&self, plugin: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .inner
            .table
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.plugin == plugin)
            .map(|b| (b.id.clone(), b.chord.clone()))
            .collect();
        out.sort();
        out
    }

    /// Starts the event thread unless it runs. A new thread first removes
    /// what stopped nodes left, then binds this node's chords again (after
    /// Hyprland restarted, it has none of them).
    fn listen(&self, hypr: &Hyprland) {
        if self.inner.listening.swap(true, Ordering::SeqCst) {
            return;
        }
        let events = match hypr.events() {
            Ok(events) => events,
            Err(e) => {
                self.inner.listening.store(false, Ordering::SeqCst);
                tracing::warn!("hotkey: cannot read Hyprland's events: {e}");
                return;
            }
        };
        sweep(hypr);
        rebind(hypr, &self.inner);
        let (inner, sink, hypr) = (self.inner.clone(), self.sink.clone(), hypr.clone());
        let spawned = std::thread::Builder::new()
            .name("bricolo-hypr-events".into())
            .spawn(move || {
                for line in events.lines() {
                    let Ok(line) = line else { break };
                    handle(&line, &hypr, &inner, &sink);
                }
                tracing::warn!("hotkey: lost Hyprland's event socket");
                inner.listening.store(false, Ordering::SeqCst);
            });
        if let Err(e) = spawned {
            self.inner.listening.store(false, Ordering::SeqCst);
            tracing::warn!("hotkey: cannot start the event thread: {e}");
        }
    }
}

fn handle(line: &str, hypr: &Hyprland, inner: &Inner, sink: &EventSink) {
    if let Some(token) = line
        .strip_prefix("custom>>")
        .and_then(|data| data.strip_prefix(EVENT))
    {
        let target = inner
            .table
            .lock()
            .unwrap()
            .iter()
            .find(|b| b.token == token)
            .map(|b| (b.plugin.clone(), b.id.clone()));
        // A bind fires on a real key, which gives the focused app the real
        // keymap back.
        super::keyboard::WTYPE_KEYMAP.store(false, std::sync::atomic::Ordering::SeqCst);
        if let Some((plugin, id)) = target {
            sink(&plugin, ProviderEvent::Hotkey(id));
        }
    } else if line.starts_with("configreloaded>>") {
        rebind(hypr, inner);
    }
}

fn rebind(hypr: &Hyprland, inner: &Inner) {
    let table = inner.table.lock().unwrap().clone();
    for b in table {
        if let Err(e) = hypr.run(&cmd::bind(
            hypr.dialect(),
            &b.keys,
            &b.token,
            &b.description(),
        )) {
            tracing::warn!(
                "hotkey: could not bind {} again for {}: {e}",
                b.chord,
                b.plugin
            );
        }
    }
}

/// Removes binds left by nodes that are no longer running.
fn sweep(hypr: &Hyprland) {
    let Ok(binds) = hypr.binds() else { return };
    for bind in binds.iter().filter(|b| stale(b, hypr.dialect())) {
        let keys = if bind.key.is_empty() {
            format!("code:{}", bind.keycode)
        } else {
            bind.key.clone()
        };
        let mods = mod_names(bind.modmask);
        let command = match hypr.dialect() {
            Dialect::Lua => {
                let mut parts = mods;
                parts.push(&keys);
                format!("eval hl.unbind({})", super::lua_quote(&parts.join(" + ")))
            }
            Dialect::Legacy => format!("keyword unbind {},{keys}", mods.join(" ")),
        };
        if hypr.run(&command).is_ok() {
            tracing::info!("hotkey: removed a bind left by a stopped node: {command}");
        }
    }
}

/// Whether a bind is one a node of this project made.
fn is_ours(bind: &Bind) -> bool {
    owner_pid(bind).is_some()
}

/// The pid of the node that made `bind`, if a node did.
fn owner_pid(bind: &Bind) -> Option<u32> {
    if let Some(rest) = bind.description.strip_prefix(DESCRIPTION) {
        let pid = rest.strip_prefix('[')?.split(']').next()?;
        return pid.parse().ok();
    }
    if bind.dispatcher == "event" {
        let token = bind.arg.strip_prefix(EVENT)?;
        return token.split('-').next()?.parse().ok();
    }
    None
}

fn stale(bind: &Bind, _: Dialect) -> bool {
    match owner_pid(bind) {
        Some(pid) if pid == std::process::id() => false,
        // SAFETY: signal 0 only checks that the process exists.
        Some(pid) => {
            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
            !alive
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_its_own_binds() {
        let lua = Bind {
            description: "bricolo[4242]: clip/show".into(),
            dispatcher: "__lua".into(),
            ..Bind::default()
        };
        assert_eq!(owner_pid(&lua), Some(4242));
        let legacy = Bind {
            dispatcher: "event".into(),
            arg: "bricolo-hotkey:99-3".into(),
            ..Bind::default()
        };
        assert_eq!(owner_pid(&legacy), Some(99));
        let user = Bind {
            description: "Volume up".into(),
            dispatcher: "__lua".into(),
            ..Bind::default()
        };
        assert_eq!(owner_pid(&user), None);
        assert!(!stale(&user, Dialect::Lua));
        let mine = Bind {
            description: format!("bricolo[{}]: a/b", std::process::id()),
            ..Bind::default()
        };
        assert!(!stale(&mine, Dialect::Lua));
    }

    #[test]
    fn delivers_presses_by_token() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();
        let sink: EventSink = Arc::new(move |plugin, event| {
            seen.lock().unwrap().push((plugin.to_string(), event));
        });
        let inner = Inner::default();
        inner.table.lock().unwrap().push(Binding {
            plugin: "clip".into(),
            id: "show".into(),
            chord: "cmd+alt+c".into(),
            keys: Keys::from_chord(&Chord::parse("cmd+alt+c").unwrap()).unwrap(),
            token: "1-0".into(),
        });
        let hypr = Hyprland::at("/nonexistent".into(), Dialect::Lua);
        handle("custom>>bricolo-hotkey:1-0", &hypr, &inner, &sink);
        handle("custom>>bricolo-hotkey:2-0", &hypr, &inner, &sink);
        handle("activewindow>>kitty,~", &hypr, &inner, &sink);
        assert_eq!(
            *got.lock().unwrap(),
            [("clip".to_string(), ProviderEvent::Hotkey("show".into()))]
        );
    }
}
