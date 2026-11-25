//! The supervisor: finds the scripts in `scripts/`, starts each in its own
//! Lua state, and reloads one script when its file, or a module it
//! loaded, changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use bricolo_providers::disk::DiskGrant;
use bricolo_providers::shell::ShellGrant;
use bricolo_providers::{Caller, Providers};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::mailbox::Mailbox;
use crate::sandbox;
use crate::script::{self, Env, Live, Modules, Running, ScriptInfo, SharedInfo, Spec, State};

/// How long the watcher waits for writes to settle before reloading.
const SETTLE: Duration = Duration::from_millis(100);

/// CPU time a script may use in one go, not counting time spent waiting on
/// calls. The node passes the `[runtime]` budgets from `node.toml`.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// For each callback, and for `dispose`.
    pub event_budget_ms: u64,
    /// For the script body together with `init`.
    pub init_budget_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            event_budget_ms: 500,
            init_budget_ms: 2000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// The `scripts/` folder. Created if missing.
    pub dir: PathBuf,
    /// Reload a script when its file changes.
    pub watch: bool,
    pub limits: Limits,
    /// Scripts turned off in the hub, by name without `.lua`. They are not
    /// started; see [`Scripts::set_enabled`].
    pub disabled: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
pub struct ScriptStatus {
    #[serde(flatten)]
    pub info: ScriptInfo,
    pub timers: usize,
    /// Bound hotkeys: id (`h1`, `h2`, in the order of `bind`) to chord.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hotkeys: BTreeMap<String, String>,
    /// For each hotkey moved to another chord in the hub, the chord the
    /// script asked for.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hotkey_defaults: BTreeMap<String, String>,
}

struct Slot {
    info: SharedInfo,
    spec: Option<Arc<Spec>>,
    running: Option<Running>,
    modules: Modules,
}

pub struct Scripts {
    env: Arc<Env>,
    mailbox: Mailbox,
    slots: tokio::sync::Mutex<BTreeMap<String, Slot>>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    disabled: Mutex<BTreeSet<String>>,
}

/// A script name: the file name without `.lua`, made of ASCII letters,
/// digits, `-` and `_`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The name a script has for providers, `kv` and logs: `<name>.lua`. A plugin's
/// name cannot contain a dot, so the two never clash.
pub fn caller_name(name: &str) -> String {
    format!("{name}.lua")
}

/// The script name of `rel`, a path relative to `scripts/`, when it is a
/// script rather than a module.
fn script_name(rel: &Path) -> Option<String> {
    if rel.components().count() != 1 || rel.extension()? != "lua" {
        return None;
    }
    let stem = rel.file_stem()?.to_str()?;
    valid_name(stem).then(|| stem.to_string())
}

/// Scripts are trusted: `disk` reaches every path.
fn everywhere() -> Vec<DiskGrant> {
    #[cfg(windows)]
    let roots: Vec<PathBuf> = ('A'..='Z')
        .map(|d| PathBuf::from(format!("{d}:\\")))
        .filter(|p| p.exists())
        .collect();
    #[cfg(not(windows))]
    let roots = vec![PathBuf::from("/")];
    roots
        .into_iter()
        .map(|root| DiskGrant { root, write: true })
        .collect()
}

impl Scripts {
    /// Starts every script in `options.dir`. `providers` are the node's
    /// routed providers, built with `mailbox.sink()` as (part of) their
    /// event sink. Must be called inside a Tokio runtime.
    pub async fn start(
        providers: Providers,
        mailbox: Mailbox,
        options: Options,
    ) -> std::io::Result<Arc<Self>> {
        std::fs::create_dir_all(&options.dir)?;
        let dir = options.dir.canonicalize()?;
        let scripts = Arc::new(Self {
            env: Arc::new(Env {
                providers,
                limits: options.limits,
                dir,
            }),
            mailbox,
            slots: tokio::sync::Mutex::default(),
            watcher: Mutex::new(None),
            disabled: Mutex::new(options.disabled.clone()),
        });
        scripts.load_all().await;
        if options.watch {
            scripts.watch(&options.dir)?;
        }
        Ok(scripts)
    }

    pub fn dir(&self) -> &Path {
        &self.env.dir
    }

    /// The scripts in `scripts/`, sorted.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.env.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                let rel = PathBuf::from(e.file_name());
                let name = script_name(&rel);
                if name.is_none() && rel.extension().is_some_and(|x| x == "lua") {
                    tracing::warn!(
                        "skipping scripts/{}: names use letters, digits, - and _",
                        rel.display()
                    );
                }
                name
            })
            .collect();
        names.sort();
        names
    }

    /// Reads `scripts/<name>.lua` and checks that it compiles.
    fn read(&self, name: &str) -> Result<Spec, String> {
        let caller_name = caller_name(name);
        let path = self.env.dir.join(&caller_name);
        let source =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let lua = mlua::Lua::new_with(mlua::StdLib::NONE, mlua::LuaOptions::default())
            .map_err(|e| e.to_string())?;
        sandbox::compile(&lua, &source, &caller_name).map_err(|e| e.to_string())?;
        Ok(Spec {
            name: name.to_string(),
            caller: Caller {
                plugin: caller_name,
                disk: everywhere(),
                shell: ShellGrant::any(),
                http: vec![bricolo_providers::http::ANY.into()],
            },
            source,
        })
    }

    async fn load_all(&self) {
        let mut slots = self.slots.lock().await;
        let mut specs = Vec::new();
        for name in self.names() {
            if self.is_disabled(&name) {
                tracing::info!(plugin = %caller_name(&name), "disabled in the hub; not started");
                slots.insert(name.clone(), disabled(&name));
                continue;
            }
            match self.read(&name) {
                Ok(spec) => specs.push(Arc::new(spec)),
                Err(reason) => {
                    tracing::error!(plugin = %caller_name(&name), "{reason}");
                    slots.insert(name.clone(), off(&name, reason));
                }
            }
        }
        // Scripts are independent, so they start side by side.
        let started =
            futures_util::future::join_all(specs.into_iter().map(|spec| self.launch(spec))).await;
        for slot in started {
            let name = slot.info.lock().unwrap().name.clone();
            slots.insert(name, slot);
        }
    }

    /// Starts `spec` and returns its slot, running or off.
    async fn launch(&self, spec: Arc<Spec>) -> Slot {
        let caller = spec.caller.plugin.clone();
        let info = Arc::new(Mutex::new(ScriptInfo {
            name: spec.name.clone(),
            state: State::Running,
            error: None,
            restarts: 0,
        }));
        let modules = Modules::default();
        let (tx, rx) = mpsc::unbounded_channel();
        self.mailbox.insert(&caller, tx.clone());
        let started = Instant::now();
        let running = match Live::start(&self.env, &spec, &modules).await {
            Ok(live) => {
                tracing::info!(plugin = %caller, "started in {} ms", started.elapsed().as_millis());
                Some(script::spawn(
                    self.env.clone(),
                    spec.clone(),
                    live,
                    info.clone(),
                    modules.clone(),
                    tx,
                    rx,
                ))
            }
            Err(reason) => {
                tracing::error!(plugin = %caller, "{reason}");
                self.mailbox.remove(&caller);
                self.env.providers.release(&caller);
                script::set_off(&info, reason);
                None
            }
        };
        Slot {
            info,
            spec: Some(spec),
            running,
            modules,
        }
    }

    async fn stop_slot(&self, name: &str, slot: &mut Slot) {
        let caller = caller_name(name);
        self.mailbox.remove(&caller);
        if let Some(running) = slot.running.take() {
            running.stop().await;
            tracing::info!(plugin = %caller, "stopped");
        }
    }

    fn is_disabled(&self, name: &str) -> bool {
        self.disabled.lock().unwrap().contains(name)
    }

    /// Turns a script off (stops it and keeps it stopped) or back on. The
    /// caller persists the choice; the node keeps it in `state.toml`.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        if !valid_name(name) || !self.env.dir.join(caller_name(name)).is_file() {
            return Err(format!(
                "no script named {name} in {}",
                self.env.dir.display()
            ));
        }
        if enabled {
            if self.disabled.lock().unwrap().remove(name) {
                tracing::info!(plugin = %caller_name(name), "enabled");
                self.reload(name).await;
            }
            return Ok(());
        }
        if !self.disabled.lock().unwrap().insert(name.to_string()) {
            return Ok(());
        }
        let mut slots = self.slots.lock().await;
        if let Some(mut slot) = slots.remove(name) {
            self.stop_slot(name, &mut slot).await;
        }
        slots.insert(name.to_string(), disabled(name));
        tracing::info!(plugin = %caller_name(name), "disabled");
        Ok(())
    }

    /// The scripts turned off in the hub.
    pub fn disabled(&self) -> BTreeSet<String> {
        self.disabled.lock().unwrap().clone()
    }

    /// Reloads one script from disk; also turns an `off` script back on.
    /// If the new file does not compile, or fails while starting, the old
    /// version keeps running. A disabled script stays off.
    pub async fn reload(&self, name: &str) {
        let mut slots = self.slots.lock().await;
        if self.is_disabled(name) {
            slots.insert(name.to_string(), disabled(name));
            return;
        }
        let caller = caller_name(name);
        let spec = match self.read(name) {
            Ok(spec) => Arc::new(spec),
            Err(reason) => {
                tracing::error!(plugin = %caller, "reload refused: {reason}");
                match slots.get(name) {
                    Some(slot) if slot.running.is_some() => {
                        slot.info.lock().unwrap().error = Some(format!("reload refused: {reason}"));
                    }
                    _ => {
                        slots.insert(name.to_string(), off(name, reason));
                    }
                }
                return;
            }
        };
        let Some(mut old) = slots.remove(name) else {
            let slot = self.launch(spec).await;
            slots.insert(name.to_string(), slot);
            return;
        };
        self.stop_slot(name, &mut old).await;
        let fresh = self.launch(spec).await;
        if fresh.running.is_some() {
            tracing::info!(plugin = %caller, "reloaded");
            slots.insert(name.to_string(), fresh);
            return;
        }
        let reason = fresh.info.lock().unwrap().error.clone().unwrap_or_default();
        // The new version failed while starting: bring the old one back.
        match old.spec.clone() {
            Some(old_spec) if old.info.lock().unwrap().state == State::Running => {
                let back = self.launch(old_spec).await;
                back.info.lock().unwrap().error = Some(format!("reload failed: {reason}"));
                slots.insert(name.to_string(), back);
            }
            _ => {
                slots.insert(name.to_string(), fresh);
            }
        }
    }

    /// Stops and forgets one script, for example after its file is deleted.
    pub async fn remove(&self, name: &str) {
        let mut slots = self.slots.lock().await;
        if let Some(mut slot) = slots.remove(name) {
            self.stop_slot(name, &mut slot).await;
        }
    }

    /// Stops every script.
    pub async fn shutdown(&self) {
        self.watcher.lock().unwrap().take();
        let mut slots = self.slots.lock().await;
        let names: Vec<String> = slots.keys().cloned().collect();
        for name in names {
            if let Some(slot) = slots.get_mut(&name) {
                self.stop_slot(&name, slot).await;
                slot.info.lock().unwrap().state = State::Stopped;
            }
        }
    }

    pub async fn status(&self) -> Vec<ScriptStatus> {
        let slots = self.slots.lock().await;
        slots
            .iter()
            .map(|(name, slot)| {
                let owner = caller_name(name);
                let hotkey = &self.env.providers.hotkey;
                ScriptStatus {
                    info: slot.info.lock().unwrap().clone(),
                    timers: self.env.providers.timer.count(&owner),
                    hotkeys: hotkey.bound(&owner).into_iter().collect(),
                    hotkey_defaults: hotkey.changed(&owner).into_iter().collect(),
                }
            })
            .collect()
    }

    pub async fn script_status(&self, name: &str) -> Option<ScriptInfo> {
        let slots = self.slots.lock().await;
        slots.get(name).map(|s| s.info.lock().unwrap().clone())
    }

    /// The scripts to reload for these changed paths, relative to
    /// `scripts/`: a changed script, and every script that loaded a
    /// changed module. A running script whose file and modules still hold
    /// what it loaded is left alone; file events can arrive late or for
    /// writes that changed nothing.
    async fn affected(&self, changed: &[PathBuf]) -> BTreeSet<String> {
        let slots = self.slots.lock().await;
        let mut names = BTreeSet::new();
        for rel in changed {
            let full = self.env.dir.join(rel);
            let now = std::fs::read(&full).ok();
            if let Some(name) = script_name(rel) {
                let same = slots.get(&name).is_some_and(|slot| {
                    slot.running.is_some() && slot.spec.as_ref().map(|s| &s.source) == now.as_ref()
                });
                if !same {
                    names.insert(name);
                }
                continue;
            }
            for (name, slot) in slots.iter() {
                let modules = slot.modules.lock().unwrap();
                if modules
                    .get(&full)
                    .is_some_and(|read| Some(read) != now.as_ref())
                {
                    names.insert(name.clone());
                }
            }
        }
        names
    }

    fn watch(self: &Arc<Self>, dir: &Path) -> std::io::Result<()> {
        use notify::Watcher;
        let roots = [self.env.dir.clone(), dir.to_path_buf()];
        let (tx, rx) = mpsc::unbounded_channel::<PathBuf>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // Linux (inotify) reports opens and reads too, and loading a
            // script reads it, so reacting to those reloads forever.
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in &event.paths {
                if let Some(rel) = roots.iter().find_map(|r| path.strip_prefix(r).ok()) {
                    let _ = tx.send(rel.to_path_buf());
                }
            }
        })
        .map_err(std::io::Error::other)?;
        watcher
            .watch(&self.env.dir, notify::RecursiveMode::Recursive)
            .map_err(std::io::Error::other)?;
        *self.watcher.lock().unwrap() = Some(watcher);
        tokio::spawn(apply_changes(Arc::downgrade(self), rx));
        Ok(())
    }
}

/// Reloads or removes each affected script once writes settle.
async fn apply_changes(scripts: Weak<Scripts>, mut rx: mpsc::UnboundedReceiver<PathBuf>) {
    while let Some(first) = rx.recv().await {
        let mut changed = vec![first];
        while let Ok(Some(path)) = tokio::time::timeout(SETTLE, rx.recv()).await {
            changed.push(path);
        }
        changed.sort();
        changed.dedup();
        let Some(scripts) = scripts.upgrade() else {
            return;
        };
        for name in scripts.affected(&changed).await {
            if scripts.env.dir.join(caller_name(&name)).is_file() {
                tracing::info!(plugin = %caller_name(&name), "file changed, reloading");
                scripts.reload(&name).await;
            } else {
                scripts.remove(&name).await;
            }
        }
    }
}

fn disabled(name: &str) -> Slot {
    let slot = off(name, String::new());
    {
        let mut info = slot.info.lock().unwrap();
        info.state = State::Disabled;
        info.error = None;
    }
    slot
}

fn off(name: &str, reason: String) -> Slot {
    Slot {
        info: Arc::new(Mutex::new(ScriptInfo {
            name: name.to_string(),
            state: State::Off,
            error: Some(reason),
            restarts: 0,
        })),
        spec: None,
        running: None,
        modules: Modules::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_top_level_lua_files_are_scripts() {
        assert_eq!(
            script_name(Path::new("speak.lua")).as_deref(),
            Some("speak")
        );
        assert_eq!(
            script_name(Path::new("my_timer-2.lua")).as_deref(),
            Some("my_timer-2")
        );
        assert_eq!(script_name(Path::new("lib/util.lua")), None);
        assert_eq!(script_name(Path::new("notes.txt")), None);
        assert_eq!(script_name(Path::new(".speak.lua.swp")), None);
        assert_eq!(script_name(Path::new(".#speak.lua")), None);
        assert_eq!(script_name(Path::new("a b.lua")), None);
    }
}
