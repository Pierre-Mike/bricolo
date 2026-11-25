//! The supervisor: scans `plugins/`, checks each plugin, starts them in
//! dependency order, routes events to them, and reloads one plugin when its
//! files change.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use bricolo_providers::disk::DiskGrant;
use bricolo_providers::log::LogLine;
use bricolo_providers::sensors::ActiveSensor;
use bricolo_providers::sidecar::SidecarStatus;
use bricolo_providers::{Caller, Desktop, EventSink, LocalOptions, ProviderEvent, Providers};
use bricolo_router::{NoPeers, Peers, Rules};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::engine::WasmEngine;
use crate::error::{Error, Result};
use crate::lock::{Lock, sha256_hex};
use crate::manifest::Manifest;
use crate::node_config::NodeConfig;
use crate::order::start_order;
use crate::paths::{Paths, home_dir};
use crate::permissions::Permissions;
use crate::plugin::{self, Cmd, Env, Live, PluginInfo, Running, SharedInfo, Spec, State};

type Senders = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Cmd>>>>;

/// How long the watcher waits for writes to settle before reloading.
const SETTLE: Duration = Duration::from_millis(300);

#[derive(Clone, Default)]
pub struct Options {
    /// Reload a plugin when its folder under `plugins/` changes.
    pub watch: bool,
    /// Receives provider events for callers that are not plugins, such as
    /// Lua scripts, which share these providers.
    pub other_events: Option<EventSink>,
    /// The main-thread loop, for hotkeys and screens. `None` in tests and
    /// on platforms without one.
    pub desktop: Option<Desktop>,
    /// Plugins turned off in the hub. They are not started; see
    /// [`Runtime::set_enabled`].
    pub disabled: BTreeSet<String>,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("watch", &self.watch)
            .field("other_events", &self.other_events.is_some())
            .field("desktop", &self.desktop)
            .field("disabled", &self.disabled)
            .finish()
    }
}

struct Slot {
    info: SharedInfo,
    spec: Option<Arc<Spec>>,
    running: Option<Running>,
}

pub struct Runtime {
    paths: Paths,
    node: NodeConfig,
    env: Arc<Env>,
    /// This node's own providers, before routing. Peers' calls use these.
    local: Providers,
    senders: Senders,
    slots: tokio::sync::Mutex<BTreeMap<String, Slot>>,
    started: Instant,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    disabled: Mutex<BTreeSet<String>>,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub node: String,
    pub pid: u32,
    pub uptime_s: u64,
    pub plugins: Vec<PluginStatus>,
    pub sidecars: Vec<SidecarStatus>,
    /// Who is using a sensor right now, such as the microphone. Scripts
    /// show as `<name>.lua`.
    pub active_sensors: Vec<ActiveSensor>,
}

#[derive(Debug, Serialize)]
pub struct PluginStatus {
    #[serde(flatten)]
    pub info: PluginInfo,
    pub timers: usize,
    /// Bound hotkeys: id to chord.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hotkeys: BTreeMap<String, String>,
    /// For each hotkey moved to another chord in the hub, the chord the
    /// plugin asked for: id to chord.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub hotkey_defaults: BTreeMap<String, String>,
}

impl Runtime {
    /// Reads the config, builds the providers, and starts every installed
    /// plugin it can. Must be called inside a Tokio runtime.
    pub async fn start(paths: Paths, options: Options) -> Result<Arc<Self>> {
        Self::start_with_peers(paths, options, Arc::new(NoPeers)).await
    }

    /// Like [`Runtime::start`], with `peers` as the router's remote targets.
    pub async fn start_with_peers(
        paths: Paths,
        options: Options,
        peers: Arc<dyn Peers>,
    ) -> Result<Arc<Self>> {
        let node = NodeConfig::load(&paths.node_toml())?;
        let rules = Rules::load(&paths.rules_toml())?;
        let engine = WasmEngine::new().map_err(Error::Invalid)?;
        let senders: Senders = Arc::default();
        let sink_senders = senders.clone();
        let other = options.other_events.clone();
        let sink: EventSink = Arc::new(move |plugin: &str, event: ProviderEvent| {
            if let Some(tx) = sink_senders.lock().unwrap().get(plugin) {
                let _ = tx.send(Cmd::Event(event.into()));
            } else if let Some(other) = &other {
                other(plugin, event);
            }
        });
        let local = Providers::local(
            LocalOptions {
                data_dir: paths.plugin_data_root(),
                logs_dir: Some(paths.logs_dir()),
                sidecars: node.sidecars.clone(),
                tts: node.tts.clone(),
                speech: node.speech.clone(),
                mic: node.mic.clone(),
                camera: node.camera.clone(),
                claude_code: node.claude_code.clone(),
                desktop: options.desktop.clone(),
                hotkeys_file: Some(paths.hotkeys_toml()),
            },
            sink,
        );
        let providers = bricolo_router::route(&rules, local.clone(), peers);
        let runtime = Arc::new(Self {
            paths,
            node,
            env: Arc::new(Env { engine, providers }),
            local,
            senders,
            slots: tokio::sync::Mutex::default(),
            started: Instant::now(),
            watcher: Mutex::new(None),
            disabled: Mutex::new(options.disabled.clone()),
        });
        runtime.load_all().await;
        if options.watch {
            runtime.watch()?;
        }
        Ok(runtime)
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The providers plugins call, with routing applied.
    pub fn providers(&self) -> &Providers {
        &self.env.providers
    }

    /// This node's own providers, without routing.
    pub fn local_providers(&self) -> &Providers {
        &self.local
    }

    async fn load_all(&self) {
        let mut names: Vec<String> = std::fs::read_dir(self.paths.plugins_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join("plugin.toml").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut slots = self.slots.lock().await;
        let mut specs = BTreeMap::new();
        for name in names {
            if self.is_disabled(&name) {
                tracing::info!(plugin = %name, "disabled in the hub; not started");
                slots.insert(name.clone(), self.disabled_slot(&name));
                continue;
            }
            match self.prepare(&name) {
                Ok(spec) => {
                    specs.insert(name, Arc::new(spec));
                }
                Err((version, reason)) => {
                    tracing::error!(plugin = %name, "refused: {reason}");
                    slots.insert(name.clone(), refused(&name, version, reason));
                }
            }
        }
        let deps = specs
            .iter()
            .map(|(n, s)| (n.clone(), s.dependencies.clone()))
            .collect();
        let (order, cyclic) = start_order(&deps);
        for (name, reason) in cyclic {
            tracing::error!(plugin = %name, "refused: {reason}");
            let version = specs[&name].version.clone();
            slots.insert(name.clone(), refused(&name, version, reason));
        }
        for name in order {
            let spec = specs
                .remove(&name)
                .expect("ordered plugins come from specs");
            let slot = self.launch(spec).await;
            slots.insert(name, slot);
        }
    }

    /// Reads and checks one installed plugin. On failure, returns the
    /// version if known, and the reason.
    fn prepare(&self, name: &str) -> Result<Spec, (String, String)> {
        let dir = self.paths.plugin_dir(name);
        let manifest =
            Manifest::load(&dir.join("plugin.toml")).map_err(|e| (String::new(), e.to_string()))?;
        let version = manifest.version.clone();
        let fail = |reason: String| (version.clone(), reason);
        if manifest.name != name {
            return Err(fail(format!(
                "plugin.toml names {:?} but the folder is {name:?}",
                manifest.name
            )));
        }
        let wasm_path = dir.join("plugin.wasm");
        let wasm =
            std::fs::read(&wasm_path).map_err(|e| fail(format!("{}: {e}", wasm_path.display())))?;
        let lock = Lock::load(&self.paths.lock_file()).map_err(|e| fail(e.to_string()))?;
        let Some(entry) = lock.plugins.get(name) else {
            return Err(fail(
                "not in plugins.lock; install it with `bricolo install`".into(),
            ));
        };
        let sha = sha256_hex(&wasm);
        if entry.sha256 != sha {
            return Err(fail(format!(
                "plugin.wasm has sha256 {sha}, but plugins.lock expects {}",
                entry.sha256
            )));
        }
        let permissions =
            Permissions::load(&self.paths.permissions_toml()).map_err(|e| fail(e.to_string()))?;
        let missing = permissions.missing(&manifest);
        if !missing.is_empty() {
            return Err(fail(format!(
                "not granted: {}; approve them with `bricolo install`",
                missing.join(", ")
            )));
        }
        let checked = self.env.engine.check(&manifest, &wasm).map_err(fail)?;
        let home = home_dir().unwrap_or_default();
        // The plugin's own data folder, which WASI already mounts at
        // `/data`, so paths `mic` returns pass `speech.transcribe-file`.
        let own = DiskGrant {
            root: self.paths.plugin_data(name),
            write: true,
        };
        let caller = Caller {
            plugin: name.to_string(),
            disk: manifest
                .permissions
                .iter()
                .filter_map(|p| DiskGrant::parse(p, &home))
                .chain([own])
                .collect(),
            shell: manifest.shell_grant(),
            http: manifest.http_allow(),
        };
        // Re-read node.toml so a reload picks up changed overrides.
        let node = NodeConfig::load(&self.paths.node_toml()).unwrap_or_else(|e| {
            tracing::warn!("{e}; using the node.toml read at startup");
            self.node.clone()
        });
        Ok(Spec {
            name: name.to_string(),
            version: manifest.version.clone(),
            checked,
            caller,
            config: node.settings_json(&manifest),
            data_dir: self.paths.plugin_data(name),
            limits: node.runtime,
            dependencies: manifest
                .dependencies()
                .into_iter()
                .map(String::from)
                .collect(),
        })
    }

    /// Starts `spec` and returns its slot, running or off.
    async fn launch(&self, spec: Arc<Spec>) -> Slot {
        let name = spec.name.clone();
        let info = Arc::new(Mutex::new(PluginInfo {
            name: name.clone(),
            version: spec.version.clone(),
            state: State::Running,
            error: None,
            restarts: 0,
        }));
        let running = match self.try_start(&spec, &info).await {
            Ok(running) => Some(running),
            Err(reason) => {
                let mut i = info.lock().unwrap();
                i.state = State::Off;
                i.error = Some(reason);
                None
            }
        };
        Slot {
            info,
            spec: Some(spec),
            running,
        }
    }

    async fn try_start(&self, spec: &Arc<Spec>, info: &SharedInfo) -> Result<Running, String> {
        let started = Instant::now();
        match Live::start(&self.env.engine, &self.env.providers, spec).await {
            Ok(live) => {
                let running = plugin::spawn(self.env.clone(), spec.clone(), live, info.clone());
                self.senders
                    .lock()
                    .unwrap()
                    .insert(spec.name.clone(), running.tx.clone());
                tracing::info!(plugin = %spec.name, version = %spec.version, "started in {} ms", started.elapsed().as_millis());
                Ok(running)
            }
            Err(reason) => {
                tracing::error!(plugin = %spec.name, "{reason}");
                self.env.providers.release(&spec.name);
                Err(reason)
            }
        }
    }

    async fn stop_slot(&self, name: &str, slot: &mut Slot) {
        self.senders.lock().unwrap().remove(name);
        if let Some(running) = slot.running.take() {
            running.stop().await;
            tracing::info!(plugin = name, "stopped");
        }
    }

    fn is_disabled(&self, name: &str) -> bool {
        self.disabled.lock().unwrap().contains(name)
    }

    /// A slot for a plugin turned off in the hub, with the version from its
    /// manifest when it reads.
    fn disabled_slot(&self, name: &str) -> Slot {
        let version = Manifest::load(&self.paths.plugin_dir(name).join("plugin.toml"))
            .map(|m| m.version)
            .unwrap_or_default();
        Slot {
            info: Arc::new(Mutex::new(PluginInfo {
                name: name.to_string(),
                version,
                state: State::Disabled,
                error: None,
                restarts: 0,
            })),
            spec: None,
            running: None,
        }
    }

    /// Turns an installed plugin off (stops it and keeps it stopped) or
    /// back on (loads it from disk). The caller persists the choice; the
    /// node keeps it in `state.toml`.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        if !self.paths.plugin_dir(name).join("plugin.toml").is_file() {
            return Err(Error::Invalid(format!(
                "no plugin named {name} is installed"
            )));
        }
        if enabled {
            if self.disabled.lock().unwrap().remove(name) {
                tracing::info!(plugin = name, "enabled");
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
        slots.insert(name.to_string(), self.disabled_slot(name));
        tracing::info!(plugin = name, "disabled");
        Ok(())
    }

    /// The plugins turned off in the hub.
    pub fn disabled(&self) -> BTreeSet<String> {
        self.disabled.lock().unwrap().clone()
    }

    /// Reloads one plugin from disk. If the new files fail their checks or
    /// `init`, the old version keeps running. A disabled plugin stays off.
    pub async fn reload(&self, name: &str) {
        let mut slots = self.slots.lock().await;
        if self.is_disabled(name) {
            slots.insert(name.to_string(), self.disabled_slot(name));
            return;
        }
        let spec = match self.prepare(name) {
            Ok(spec) => Arc::new(spec),
            Err((version, reason)) => {
                tracing::error!(plugin = name, "reload refused: {reason}");
                match slots.get(name) {
                    Some(slot) if slot.running.is_some() => {
                        slot.info.lock().unwrap().error = Some(format!("reload refused: {reason}"));
                    }
                    _ => {
                        slots.insert(name.to_string(), refused(name, version, reason));
                    }
                }
                return;
            }
        };
        if let Some(missing) = spec
            .dependencies
            .iter()
            .find(|d| !slots.get(*d).is_some_and(|s| s.running.is_some()))
        {
            let reason = format!("depends on {missing}, which is not running");
            tracing::error!(plugin = name, "reload refused: {reason}");
            if !slots.get(name).is_some_and(|s| s.running.is_some()) {
                slots.insert(
                    name.to_string(),
                    refused(name, spec.version.clone(), reason),
                );
            }
            return;
        }
        let Some(mut old) = slots.remove(name) else {
            let slot = self.launch(spec).await;
            slots.insert(name.to_string(), slot);
            return;
        };
        self.stop_slot(name, &mut old).await;
        let fresh = self.launch(spec).await;
        if fresh.running.is_some() {
            tracing::info!(plugin = name, "reloaded");
            slots.insert(name.to_string(), fresh);
            return;
        }
        let reason = fresh.info.lock().unwrap().error.clone().unwrap_or_default();
        // The new version failed in init: bring the old one back.
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

    /// Stops and forgets one plugin, for example after `bricolo uninstall`.
    pub async fn remove(&self, name: &str) {
        let mut slots = self.slots.lock().await;
        if let Some(mut slot) = slots.remove(name) {
            self.stop_slot(name, &mut slot).await;
        }
    }

    /// Stops every plugin, dependents first, then the sidecars.
    pub async fn shutdown(&self) {
        let mut slots = self.slots.lock().await;
        let deps = slots
            .iter()
            .map(|(n, s)| {
                (
                    n.clone(),
                    s.spec
                        .as_ref()
                        .map(|s| s.dependencies.clone())
                        .unwrap_or_default(),
                )
            })
            .collect();
        let (mut order, _) = start_order(&deps);
        order.reverse();
        for name in order {
            if let Some(slot) = slots.get_mut(&name) {
                self.stop_slot(&name, slot).await;
                slot.info.lock().unwrap().state = State::Stopped;
            }
        }
        self.watcher.lock().unwrap().take();
        self.env.providers.shutdown().await;
    }

    /// Queues `event` for `plugin`.
    pub fn fire(&self, plugin: &str, event: ProviderEvent) -> Result<()> {
        let senders = self.senders.lock().unwrap();
        let tx = senders
            .get(plugin)
            .ok_or_else(|| Error::Invalid(format!("{plugin} is not running")))?;
        tx.send(Cmd::Event(event.into()))
            .map_err(|_| Error::Invalid(format!("{plugin} is not running")))
    }

    pub async fn status(&self) -> Status {
        let slots = self.slots.lock().await;
        let plugins = slots
            .iter()
            .map(|(name, slot)| PluginStatus {
                info: slot.info.lock().unwrap().clone(),
                timers: self.env.providers.timer.count(name),
                hotkeys: self.env.providers.hotkey.bound(name).into_iter().collect(),
                hotkey_defaults: self
                    .env
                    .providers
                    .hotkey
                    .changed(name)
                    .into_iter()
                    .collect(),
            })
            .collect();
        Status {
            node: self.node.name.clone(),
            pid: std::process::id(),
            uptime_s: self.started.elapsed().as_secs(),
            plugins,
            sidecars: self.env.providers.sidecars.status(),
            active_sensors: self.local.sensors.active(),
        }
    }

    pub async fn plugin_status(&self, name: &str) -> Option<PluginInfo> {
        let slots = self.slots.lock().await;
        slots.get(name).map(|s| s.info.lock().unwrap().clone())
    }

    /// The last `limit` lines plugins wrote, from one plugin or from all.
    pub fn recent_logs(&self, plugin: Option<&str>, limit: usize) -> Vec<LogLine> {
        self.env.providers.log.recent(plugin, limit)
    }

    fn watch(self: &Arc<Self>) -> Result<()> {
        use notify::Watcher;
        let dir = self.paths.plugins_dir();
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let roots = [
            dir.clone(),
            dir.canonicalize().unwrap_or_else(|_| dir.clone()),
        ];
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // Linux (inotify) reports opens and reads too, and loading a
            // plugin reads its files, so reacting to those reloads forever.
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in &event.paths {
                if let Some(name) = roots
                    .iter()
                    .find_map(|r| path.strip_prefix(r).ok())
                    .and_then(|rest| rest.components().next())
                {
                    let _ = tx.send(name.as_os_str().to_string_lossy().into_owned());
                }
            }
        })
        .map_err(|e| Error::Invalid(format!("cannot watch {}: {e}", dir.display())))?;
        watcher
            .watch(&dir, notify::RecursiveMode::Recursive)
            .map_err(|e| Error::Invalid(format!("cannot watch {}: {e}", dir.display())))?;
        *self.watcher.lock().unwrap() = Some(watcher);
        tokio::spawn(apply_changes(Arc::downgrade(self), rx));
        Ok(())
    }
}

/// Reloads or removes each changed plugin once its writes settle.
async fn apply_changes(runtime: Weak<Runtime>, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(first) = rx.recv().await {
        let mut names = vec![first];
        while let Ok(Some(name)) = tokio::time::timeout(SETTLE, rx.recv()).await {
            names.push(name);
        }
        names.sort();
        names.dedup();
        let Some(runtime) = runtime.upgrade() else {
            return;
        };
        for name in names {
            if name.starts_with('.') {
                continue;
            }
            if runtime
                .paths
                .plugin_dir(&name)
                .join("plugin.toml")
                .is_file()
            {
                tracing::info!(plugin = %name, "files changed, reloading");
                runtime.reload(&name).await;
            } else {
                runtime.remove(&name).await;
            }
        }
    }
}

fn refused(name: &str, version: String, reason: String) -> Slot {
    Slot {
        info: Arc::new(Mutex::new(PluginInfo {
            name: name.to_string(),
            version,
            state: State::Refused,
            error: Some(reason),
            restarts: 0,
        })),
        spec: None,
        running: None,
    }
}
