//! `bricolo run` and the control socket: a Unix socket at `<data>/bricolo.sock`, or
//! on Windows a named pipe named after the data folder, speaking JSON
//! lines. Each request is one line, like
//! `{"cmd":"status"}`, `{"cmd":"reload","plugin":"hello"}` or
//! `{"cmd":"fire","plugin":"hello","hotkey":"greet"}`; each answer is one
//! line of JSON, with an `"error"` key when it failed.
//!
//! Other requests: `{"cmd":"reload-script","script":"speak"}`,
//! `{"cmd":"reload-peers"}` rereads peers.toml, `{"cmd":"reload-sidecars"}`
//! rereads `[sidecars.*]` in node.toml and starts, stops or restarts what
//! changed, and
//! `{"cmd":"call","capability":"disk","method":"read","args":["~/a.txt"],
//! "grants":["disk:~"]}` runs one routed capability call, for debugging.
//!
//! For the UI: `{"cmd":"enable","plugin":"hello"}` and `{"cmd":"disable",
//! "script":"speak"}` turn one on or off and remember it in `state.toml`;
//! `{"cmd":"restart-sidecar","sidecar":"parakeet"}`;
//! `{"cmd":"set-hotkey","script":"dictation","id":"h1","chord":"cmd+shift+d"}`
//! moves a binding to another chord (`"chord":null` puts it back) and
//! remembers it in `hotkeys.toml`;
//! `{"cmd":"settings","plugin":"p"}` and `{"cmd":"set-settings","plugin":"p",
//! "values":{"voice":"marius"}}` read and write a plugin's settings (see
//! `settings.rs`);
//! `{"cmd":"ui-action","action":{"kind":"switch","owner":"p","id":"x",
//! "on":true}}` hands a tile or panel interaction to its owner;
//! `{"cmd":"sensor","owner":"p","sensor":"mic","active":true}` marks a
//! sensor in use by hand, to test the recording dot. `{"cmd":"subscribe"}`
//! turns the connection into a stream of events (see `subscribe.rs`).

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bricolo_lua::{Mailbox, Scripts};
use bricolo_mesh::capabilities::dispatch;
use bricolo_mesh::wire::Json;
use bricolo_mesh::{Mesh, PeerList};
use bricolo_providers::disk::DiskGrant;
use bricolo_providers::permissions;
use bricolo_providers::sensors::Sensor;
use bricolo_providers::ui::UiAction;
use bricolo_providers::{CallError, Caller, Desktop};
use bricolo_router::{NoPeers, Peers, Rules};
use bricolo_runtime::manifest::Manifest;
use bricolo_runtime::node_config::NodeConfig;
use bricolo_runtime::paths::home_dir;
use bricolo_runtime::{Options, Paths, ProviderEvent, Runtime};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::NOT_RUNNING;
use crate::library;
use crate::state::NodeState;
use crate::{settings, sidecar};

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Status,
    Reload {
        plugin: String,
    },
    ReloadScript {
        script: String,
    },
    ReloadPeers,
    /// What each mesh transport sees on its network.
    Discover,
    /// Fetches a plugin from a peer by the sha256 of its component, checks
    /// it, and answers the path of the saved bundle.
    FetchPlugin {
        peer: String,
        sha256: String,
    },
    /// Rereads node.toml and applies its `[sidecars.*]`.
    ReloadSidecars,
    Call {
        capability: String,
        method: String,
        /// One JSON value per argument. Missing means no arguments.
        #[serde(default)]
        args: Vec<Value>,
        /// `disk:` grants for this call, as a plugin would hold them.
        #[serde(default)]
        grants: Vec<String>,
        /// Who asks, for the log: `mcp` for the MCP gateway. Missing means
        /// `bricolo call`.
        #[serde(default)]
        client: Option<String>,
    },
    /// The OS permissions the node holds; with `request`, asks the OS for
    /// what is missing first: Accessibility and Input Monitoring, or only
    /// `grant` when it is set.
    Permissions {
        #[serde(default)]
        request: bool,
        #[serde(default)]
        grant: Option<String>,
    },
    /// The standard library, who may call each capability, and the OS
    /// grants: the hub's Library view.
    Library,
    /// Opens the System Settings pane for one grant.
    OpenSettings {
        grant: String,
    },
    /// Delivers `hotkey(<id>)` to a plugin, as if the chord was pressed.
    Fire {
        plugin: String,
        hotkey: String,
    },
    /// Streams status, UI and log events on this connection until it closes.
    Subscribe,
    /// Adds the MCP gateway to Claude Code or Claude Desktop: the hub's
    /// Connect buttons.
    McpConnect {
        client: crate::mcp::Client,
    },
    /// Turns the connection into an MCP session with the node's gateway:
    /// one JSON-RPC message per line each way, until it closes.
    McpSession,
    /// Turns a plugin or a script back on.
    Enable {
        #[serde(default)]
        plugin: Option<String>,
        #[serde(default)]
        script: Option<String>,
    },
    /// Turns a plugin or a script off, across restarts.
    Disable {
        #[serde(default)]
        plugin: Option<String>,
        #[serde(default)]
        script: Option<String>,
    },
    RestartSidecar {
        sidecar: String,
    },
    /// Moves a plugin's or script's hotkey to another chord, across
    /// restarts. No chord puts back the one it asked for.
    SetHotkey {
        #[serde(default)]
        plugin: Option<String>,
        #[serde(default)]
        script: Option<String>,
        id: String,
        #[serde(default)]
        chord: Option<String>,
    },
    /// A plugin's settings: defaults, hints and node.toml overrides.
    Settings {
        plugin: String,
    },
    /// Writes `[plugins.<plugin>]` in node.toml and reloads the plugin.
    /// A `null` value puts back the default.
    SetSettings {
        plugin: String,
        values: serde_json::Map<String, Value>,
    },
    /// What the user did with a tile or panel.
    UiAction {
        action: UiAction,
    },
    /// Marks a sensor in use or not, by hand. For testing the UI.
    Sensor {
        #[serde(alias = "owner")]
        plugin: String,
        sensor: Sensor,
        active: bool,
    },
}

/// What a running node holds: the two tiers, and the mesh unless it is
/// off. Scripts share the plugins' routed providers.
pub struct Node {
    pub runtime: Arc<Runtime>,
    pub scripts: Arc<Scripts>,
    pub mesh: Option<Mesh>,
    /// The MCP gateway, unless `[mcp] enabled = false`.
    pub gateway: Option<crate::gateway::Gateway>,
    /// Bumped after a request changes something, so subscribers send the
    /// new status at once instead of at their next tick.
    pub changed: watch::Sender<u64>,
    /// `state.toml`, written after each enable or disable.
    state: tokio::sync::Mutex<NodeState>,
    /// `rules.toml` as the router read it at start, for the hub.
    rules: Rules,
}

/// How [`Node::start`] runs. The default is what tests want: no watcher,
/// no mesh, no main-thread loop.
#[derive(Clone, Debug, Default)]
pub struct NodeOptions {
    /// Reload plugins and scripts when their files change.
    pub watch: bool,
    /// Start the mesh, unless `node.toml` turns it off. Tests leave it off
    /// to stay off the network.
    pub mesh: bool,
    /// The main-thread loop from `desktop::run`, for hotkeys and screens.
    pub desktop: Option<Desktop>,
}

impl Node {
    /// Starts the mesh, then the plugins, then the scripts.
    pub async fn start(paths: &Paths, options: NodeOptions) -> Result<Self> {
        let rules = Rules::load(&paths.rules_toml())?;
        let state = NodeState::load(&paths.state_toml()).unwrap_or_else(|e| {
            tracing::warn!("{e:#}; starting everything");
            NodeState::default()
        });
        let mesh = if options.mesh {
            crate::mesh::start(paths, Duration::from_secs(rules.timeout_secs)).await?
        } else {
            None
        };
        let started = match Self::start_tiers(paths, options, &state, mesh.clone()).await {
            Ok(tiers) => Self::start_gateway(paths, &tiers.0, mesh.clone())
                .await
                .map(|gateway| (tiers, gateway)),
            Err(e) => Err(e),
        };
        match started {
            Ok(((runtime, scripts), gateway)) => Ok(Self {
                runtime,
                scripts,
                mesh,
                gateway,
                changed: watch::channel(0).0,
                state: tokio::sync::Mutex::new(state),
                rules,
            }),
            Err(e) => {
                if let Some(mesh) = &mesh {
                    mesh.close().await;
                }
                Err(e)
            }
        }
    }

    async fn start_gateway(
        paths: &Paths,
        runtime: &Arc<Runtime>,
        mesh: Option<Mesh>,
    ) -> Result<Option<crate::gateway::Gateway>> {
        let config = NodeConfig::load(&paths.node_toml())?;
        let mcp = crate::gateway::McpConfig::from_table(&config.mcp)?;
        let gateway = crate::gateway::start(paths, &config.name, mcp, runtime.clone(), mesh).await;
        if gateway.is_err() {
            runtime.shutdown().await;
        }
        gateway
    }

    async fn start_tiers(
        paths: &Paths,
        options: NodeOptions,
        state: &NodeState,
        mesh: Option<Mesh>,
    ) -> Result<(Arc<Runtime>, Arc<Scripts>)> {
        let peers: Arc<dyn Peers> = match &mesh {
            Some(mesh) => Arc::new(mesh.clone()),
            None => Arc::new(NoPeers),
        };
        let mailbox = Mailbox::default();
        let watch = options.watch;
        let runtime_options = Options {
            watch,
            other_events: Some(mailbox.sink()),
            desktop: options.desktop,
            disabled: state.disabled_plugins.clone(),
        };
        let runtime = Runtime::start_with_peers(paths.clone(), runtime_options, peers).await?;
        if let Some(mesh) = &mesh {
            mesh.serve(runtime.local_providers().clone());
            mesh.share_plugins(Arc::new(crate::share::LockStore(paths.clone())));
        }
        let limits = NodeConfig::load(&paths.node_toml())?.runtime;
        let scripts = Scripts::start(
            runtime.providers().clone(),
            mailbox,
            bricolo_lua::Options {
                dir: paths.scripts_dir(),
                watch,
                limits: bricolo_lua::Limits {
                    event_budget_ms: limits.event_budget_ms,
                    init_budget_ms: limits.init_budget_ms,
                },
                disabled: state.disabled_scripts.clone(),
            },
        )
        .await;
        match scripts {
            Ok(scripts) => Ok((runtime, scripts)),
            Err(e) => {
                runtime.shutdown().await;
                Err(e).with_context(|| {
                    format!("cannot start scripts in {}", paths.scripts_dir().display())
                })
            }
        }
    }

    /// Scripts first: they may call providers that plugins also use. The
    /// runtime then stops the plugins and the sidecars. The mesh goes last,
    /// so peers see this node leave at once.
    pub async fn shutdown(&self) {
        self.scripts.shutdown().await;
        self.runtime.shutdown().await;
        if let Some(mesh) = &self.mesh {
            mesh.close().await;
        }
    }

    /// Everything `bricolo status` and the hub show.
    pub async fn status(&self) -> Value {
        let mut status = serde_json::to_value(self.runtime.status().await).unwrap_or_default();
        status["scripts"] = serde_json::to_value(self.scripts.status().await).unwrap_or_default();
        status["running"] = json!(true);
        status["mesh"] = match &self.mesh {
            Some(mesh) => {
                let mut mesh = serde_json::to_value(mesh.status()).unwrap_or_default();
                mesh["enabled"] = json!(true);
                mesh
            }
            None => json!({ "enabled": false }),
        };
        status["mcp"] = match &self.gateway {
            Some(g) => json!({ "enabled": true, "http": g.http.map(|a| a.to_string()) }),
            None => json!({ "enabled": false }),
        };
        status["rules"] = json!({
            "timeout_secs": self.rules.timeout_secs,
            "routes": self.rules.routes,
        });
        status
    }

    /// The answer to `{"cmd":"library"}`.
    pub async fn library(&self) -> Value {
        let installed: Vec<String> = self
            .runtime
            .status()
            .await
            .plugins
            .into_iter()
            .map(|p| p.info.name)
            .collect();
        let approved = bricolo_runtime::permissions::Permissions::load(
            &self.runtime.paths().permissions_toml(),
        )
        .unwrap_or_default();
        let plugin_grants = installed
            .into_iter()
            .map(|name| {
                let granted = approved.granted(&name).to_vec();
                (name, granted)
            })
            .collect();
        let scripts = self
            .scripts
            .status()
            .await
            .into_iter()
            .map(|s| s.info.name)
            .collect();
        let exe = std::env::current_exe().unwrap_or_default();
        let term = std::env::var("TERM_PROGRAM").ok();
        library::answer(&library::Facts {
            capabilities: bricolo_providers::catalog::capabilities(),
            held: permissions::check(),
            not_asked: permissions::not_asked(),
            holder: library::holder(&exe, term.as_deref()),
            plugin_grants,
            scripts,
        })
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    /// Turns a plugin or script on or off and records it in `state.toml`.
    async fn set_enabled(
        &self,
        plugin: Option<String>,
        script: Option<String>,
        enabled: bool,
    ) -> Value {
        let mut state = self.state.lock().await;
        let result = match (&plugin, &script) {
            (Some(name), None) => self
                .runtime
                .set_enabled(name, enabled)
                .await
                .map(|()| state.disabled_plugins = self.runtime.disabled())
                .map_err(|e| e.to_string()),
            (None, Some(name)) => self
                .scripts
                .set_enabled(name, enabled)
                .await
                .map(|()| state.disabled_scripts = self.scripts.disabled()),
            _ => Err("name either a plugin or a script".into()),
        };
        if let Err(e) = result {
            return json!({ "error": e });
        }
        self.bump();
        if let Err(e) = state.save(&self.runtime.paths().state_toml()) {
            return json!({ "error": format!("{e:#}") });
        }
        let status = match (&plugin, &script) {
            (Some(name), _) => serde_json::to_value(self.runtime.plugin_status(name).await),
            (_, Some(name)) => serde_json::to_value(self.scripts.script_status(name).await),
            _ => Ok(Value::Null),
        };
        json!({ "enabled": enabled, "status": status.unwrap_or_default() })
    }
}

pub async fn handle(node: &Node, line: &str) -> Value {
    let runtime = &node.runtime;
    match serde_json::from_str::<Request>(line) {
        Ok(Request::Status) => node.status().await,
        Ok(Request::Reload { plugin }) => {
            node.runtime.reload(&plugin).await;
            node.bump();
            json!({ "reloaded": plugin, "status": node.runtime.plugin_status(&plugin).await })
        }
        Ok(Request::ReloadScript { script }) => {
            node.scripts.reload(&script).await;
            node.bump();
            json!({ "reloaded": script, "status": node.scripts.script_status(&script).await })
        }
        Ok(Request::McpConnect { client }) => match crate::mcp::connect_quietly(client) {
            Ok(message) => json!({ "connected": true, "message": message }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        },
        Ok(Request::McpSession) => {
            json!({ "error": "mcp-session must be the first line of its own connection" })
        }
        Ok(Request::Subscribe) => {
            json!({ "error": "subscribe must be the first line of its own connection" })
        }
        Ok(Request::Enable { plugin, script }) => node.set_enabled(plugin, script, true).await,
        Ok(Request::Disable { plugin, script }) => node.set_enabled(plugin, script, false).await,
        Ok(Request::RestartSidecar { sidecar }) => {
            if runtime.local_providers().sidecars.restart(&sidecar) {
                node.bump();
                json!({ "restarting": sidecar })
            } else {
                json!({ "error": format!("no sidecar named {sidecar} in node.toml") })
            }
        }
        Ok(Request::SetHotkey {
            plugin,
            script,
            id,
            chord,
        }) => {
            let owner = match (plugin, script) {
                (Some(name), None) => name,
                (None, Some(name)) => bricolo_lua::caller_name(&name),
                _ => return json!({ "error": "name either a plugin or a script" }),
            };
            match runtime
                .local_providers()
                .hotkey
                .rebind(&owner, &id, chord)
                .await
            {
                Ok(chord) => {
                    node.bump();
                    json!({ "hotkey": id, "owner": owner, "chord": chord })
                }
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        Ok(Request::Settings { plugin }) => match plugin_settings(node, &plugin) {
            Ok(answer) => answer,
            Err(e) => json!({ "error": e }),
        },
        Ok(Request::SetSettings { plugin, values }) => {
            let file = runtime.paths().node_toml();
            let saved = (|| -> Result<(), String> {
                let (manifest, _) = load_manifest(node, &plugin)?;
                let text = sidecar::read(&file).map_err(|e| format!("{e:#}"))?;
                let edited = settings::edit(&text, &manifest, &values)?;
                sidecar::write(&file, &edited).map_err(|e| format!("{e:#}"))
            })();
            if let Err(e) = saved {
                return json!({ "error": e });
            }
            runtime.reload(&plugin).await;
            node.bump();
            let mut answer =
                plugin_settings(node, &plugin).unwrap_or_else(|e| json!({ "error": e }));
            answer["status"] = json!(runtime.plugin_status(&plugin).await);
            answer
        }
        Ok(Request::UiAction { action }) => match runtime.local_providers().ui.act(action) {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "error": e.to_string() }),
        },
        Ok(Request::Sensor {
            plugin,
            sensor,
            active,
        }) => {
            runtime
                .local_providers()
                .sensors
                .set(&plugin, sensor, active);
            json!({ "active_sensors": runtime.local_providers().sensors.active() })
        }
        Ok(Request::ReloadSidecars) => reload_sidecars(node).await,
        Ok(Request::ReloadPeers) => {
            let Some(mesh) = &node.mesh else {
                return json!({ "error": "the mesh is disabled in node.toml" });
            };
            match PeerList::load(&runtime.paths().peers_toml()) {
                Ok(list) => {
                    let names: Vec<String> = list.peers.keys().cloned().collect();
                    mesh.set_peers(list);
                    json!({ "peers": names })
                }
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        Ok(Request::Discover) => {
            let Some(mesh) = &node.mesh else {
                return json!({ "error": "the mesh is disabled in node.toml" });
            };
            let found: Vec<Value> = mesh
                .discover()
                .await
                .into_iter()
                .map(|(kind, result)| match result {
                    Ok(peers) => json!({ "transport": kind, "peers": peers }),
                    Err(e) => json!({ "transport": kind, "error": e }),
                })
                .collect();
            json!({ "transports": found })
        }
        Ok(Request::FetchPlugin { peer, sha256 }) => {
            let Some(mesh) = &node.mesh else {
                return json!({ "error": "the mesh is disabled in node.toml" });
            };
            match crate::share::fetch(mesh, runtime.paths(), &peer, &sha256).await {
                Ok(path) => json!({ "path": path }),
                Err(e) => json!({ "error": e }),
            }
        }
        Ok(Request::Call {
            capability,
            method,
            args,
            grants,
            client,
        }) => {
            let client = client.as_deref().unwrap_or("call");
            call(runtime, client, &capability, &method, &args, &grants).await
        }
        Ok(Request::Permissions { request, grant }) => {
            let held = match (request, grant) {
                (true, Some(grant)) => {
                    if permissions::grant(&grant).is_none() {
                        return json!({ "error": format!("no grant named {grant}") });
                    }
                    permissions::request_grant(&grant)
                }
                (true, None) => permissions::request(),
                (false, _) => permissions::check(),
            };
            // Which capabilities each grant serves, so `bricolo permissions`
            // tells the user what a missing grant turns off.
            let used_by: serde_json::Map<String, Value> = permissions::GRANTS
                .iter()
                .map(|g| {
                    (
                        g.name.to_string(),
                        json!(bricolo_providers::catalog::needing(g.name)),
                    )
                })
                .collect();
            json!({ "permissions": held, "complete": held.complete(), "used_by": used_by })
        }
        Ok(Request::Library) => node.library().await,
        Ok(Request::OpenSettings { grant }) => {
            if cfg!(test) {
                // Tests never open System Settings.
                library::open_settings(&grant, |_| Ok(()))
            } else {
                library::open_settings(&grant, library::open_url)
            }
        }
        Ok(Request::Fire { plugin, hotkey }) => {
            match runtime.fire(&plugin, ProviderEvent::Hotkey(hotkey.clone())) {
                Ok(()) => json!({ "fired": hotkey, "plugin": plugin }),
                Err(e) => json!({ "error": e.to_string() }),
            }
        }
        Err(e) => json!({ "error": format!("bad request: {e}") }),
    }
}

/// Serves one MCP session on a control connection until it closes.
async fn mcp_session<R, W>(node: &Node, read: R, mut write: W)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    match &node.gateway {
        Some(g) => {
            if let Err(e) = bricolo_mcp::stdio::serve(g.server.clone(), read, write).await {
                tracing::debug!("MCP session ended: {e}");
            }
        }
        None => {
            use tokio::io::AsyncWriteExt;
            let answer = json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32000, "message": "the MCP gateway is off: [mcp] enabled = false in node.toml" },
            });
            let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
        }
    }
}

/// An installed plugin's manifest and the text of its plugin.toml.
fn load_manifest(node: &Node, plugin: &str) -> Result<(Manifest, String), String> {
    let file = node.runtime.paths().plugin_dir(plugin).join("plugin.toml");
    let text = std::fs::read_to_string(&file)
        .map_err(|_| format!("no plugin named {plugin} is installed"))?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok((manifest, text))
}

/// The answer to `{"cmd":"settings"}`.
fn plugin_settings(node: &Node, plugin: &str) -> Result<Value, String> {
    let (manifest, text) = load_manifest(node, plugin)?;
    let config = NodeConfig::load(&node.runtime.paths().node_toml()).map_err(|e| e.to_string())?;
    let order = settings::order(&text, &manifest);
    Ok(settings::describe(
        &manifest,
        &order,
        config.plugins.get(plugin),
    ))
}

/// Rereads node.toml and makes the sidecars match it. `[tts]` and
/// `[speech]` keep the server they read at start.
async fn reload_sidecars(node: &Node) -> Value {
    let runtime = &node.runtime;
    let config = match NodeConfig::load(&runtime.paths().node_toml()) {
        Ok(config) => config,
        Err(e) => return json!({ "error": e.to_string() }),
    };
    let report = runtime
        .local_providers()
        .sidecars
        .reload(config.sidecars)
        .await;
    node.bump();
    json!({ "sidecars": report })
}

/// Runs one call through the routed providers, as a plugin named
/// `bricolo-<client>` (`bricolo-call`, `bricolo-mcp`) holding `grants`, and logs it.
async fn call(
    runtime: &Runtime,
    client: &str,
    capability: &str,
    method: &str,
    args: &[Value],
    grants: &[String],
) -> Value {
    let home = home_dir().unwrap_or_default();
    let mut disk = Vec::new();
    for g in grants {
        match DiskGrant::parse(g, &home) {
            Some(grant) => disk.push(grant),
            None => {
                return json!({ "error": format!("bad grant {g:?}; use disk:<path> or disk:read:<path>") });
            }
        }
    }
    let caller = Caller {
        plugin: format!("bricolo-{client}"),
        disk,
        ..Caller::default()
    };
    let args = serde_json::to_vec(args).unwrap_or_default();
    let outcome = dispatch::<Json>(runtime.providers(), &caller, capability, method, &args).await;
    tracing::info!(
        target: "audit",
        client = %caller.plugin,
        capability,
        method,
        outcome = match &outcome { Ok(_) => "ok", Err(e) => error_name(e) },
        "capability call"
    );
    match outcome {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let mut answer = json!({ "ok": value });
            if let Some(text) = as_text(&answer["ok"]) {
                answer["text"] = json!(text);
            }
            answer
        }
        Err(e) => json!({ "error": e.to_string(), "call_error": error_name(&e) }),
    }
}

/// Bytes that are UTF-8, shown as text too.
fn as_text(value: &Value) -> Option<String> {
    let bytes: Vec<u8> = value
        .as_array()?
        .iter()
        .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
        .collect::<Option<_>>()?;
    String::from_utf8(bytes).ok()
}

fn error_name(e: &CallError) -> &'static str {
    match e {
        CallError::Unavailable => "unavailable",
        CallError::Timeout => "timeout",
        CallError::Denied => "denied",
        CallError::Failed(_) => "failed",
    }
}

/// Runs the node until Ctrl+C or SIGTERM (on Windows: Ctrl+C, Ctrl+Break,
/// or the console closing, logoff or shutdown), then disposes every script and
/// plugin, stops the sidecars and closes the mesh.
pub async fn run_node(paths: Paths, desktop: Option<Desktop>) -> Result<()> {
    let _guard = crate::logs::init(&paths)?;
    for dir in [paths.plugins_dir(), paths.scripts_dir(), paths.data.clone()] {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let listener = imp::bind(&paths).await?;
    let node = Arc::new(
        Node::start(
            &paths,
            NodeOptions {
                watch: true,
                mesh: true,
                desktop,
            },
        )
        .await?,
    );
    let status = node.runtime.status().await;
    tracing::info!(
        node = %status.node,
        config = %paths.config.display(),
        data = %paths.data.display(),
        "running with {} plugins and {} scripts",
        status.plugins.len(),
        node.scripts.status().await.len()
    );
    let held = permissions::check();
    if !held.complete() {
        tracing::warn!(
            ?held,
            "macOS permissions are missing; run `bricolo permissions --request`"
        );
    }
    let server = tokio::spawn(imp::serve(listener, node.clone()));
    imp::shutdown_signal().await;
    tracing::info!("stopping");
    node.shutdown().await;
    server.abort();
    imp::remove(&paths);
    Ok(())
}

/// Prints the running node's status, or `{"running": false}` and exits 2.
pub async fn status(paths: &Paths) -> Result<ExitCode> {
    send(paths, json!({ "cmd": "status" })).await
}

/// Prints the permissions the running node holds. With `request`, the node
/// asks macOS for what is missing, and the settings panes for it open.
pub async fn permissions(paths: &Paths, request: bool) -> Result<ExitCode> {
    let ask = json!({ "cmd": "permissions", "request": request });
    let answer = match imp::request(paths, &ask).await {
        Ok(answer) => answer,
        Err(_) => return send(paths, ask).await,
    };
    println!("{}", serde_json::to_string_pretty(&answer)?);
    if request {
        for pane in settings_panes(&answer) {
            let _ = library::open_url(&permissions::settings_url(pane));
        }
    }
    Ok(if answer["complete"] == json!(true) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The System Settings panes that turn on what `answer` lacks of the
/// grants `--request` asks for: Accessibility, posting events and Input
/// Monitoring.
fn settings_panes(answer: &Value) -> Vec<&'static str> {
    let held = &answer["permissions"];
    let mut panes: Vec<&'static str> = Vec::new();
    for name in ["accessibility", "post_events", "input_monitoring"] {
        if held[name] == json!(false)
            && let Some(g) = permissions::grant(name)
            && !panes.contains(&g.pane)
        {
            panes.push(g.pane);
        }
    }
    panes
}

/// Sends one request to the running node and prints the answer. Exits 2
/// when no node answers, 1 when the answer is an error.
pub async fn send(paths: &Paths, request: Value) -> Result<ExitCode> {
    match imp::request(paths, &request).await {
        Ok(answer) => {
            println!("{}", serde_json::to_string_pretty(&answer)?);
            if answer.get("error").is_some() {
                return Ok(ExitCode::FAILURE);
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            println!("{}", json!({ "running": false }));
            eprintln!("no node answers on {}: {e:#}", paths.socket().display());
            Ok(ExitCode::from(NOT_RUNNING))
        }
    }
}

/// Sends one request to the running node.
pub async fn request(paths: &Paths, request: &Value) -> Result<Value> {
    imp::request(paths, request).await
}

/// The two halves of a control connection.
pub type Conn = (
    Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
);

/// A raw connection to the running node's control socket.
pub async fn connect(paths: &Paths) -> Result<Conn> {
    imp::connect(paths).await
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use anyhow::{Context, Result, bail};
    use bricolo_runtime::Paths;
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    /// Binds the socket, removing a stale one. Fails if a node answers.
    pub async fn bind(paths: &Paths) -> Result<UnixListener> {
        let path = paths.socket();
        if path.exists() {
            if UnixStream::connect(&path).await.is_ok() {
                bail!("a node is already running on {}", path.display());
            }
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove {}", path.display()))?;
        }
        let listener =
            UnixListener::bind(&path).with_context(|| format!("cannot bind {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }

    use super::Node;

    pub async fn serve(listener: UnixListener, node: Arc<Node>) {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let node = node.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if matches!(serde_json::from_str(&line), Ok(super::Request::McpSession)) {
                        super::mcp_session(&node, lines.into_inner(), write).await;
                        break;
                    }
                    if matches!(serde_json::from_str(&line), Ok(super::Request::Subscribe)) {
                        // Streams until the client hangs up.
                        tokio::select! {
                            _ = crate::subscribe::stream(&node, &mut write) => {}
                            _ = async { while let Ok(Some(_)) = lines.next_line().await {} } => {}
                        }
                        break;
                    }
                    let answer = super::handle(&node, &line).await;
                    let mut out = answer.to_string();
                    out.push('\n');
                    if write.write_all(out.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    }

    pub async fn connect(paths: &Paths) -> Result<super::Conn> {
        let stream = UnixStream::connect(paths.socket()).await?;
        let (read, write) = stream.into_split();
        Ok((Box::new(read), Box::new(write)))
    }

    pub async fn request(paths: &Paths, request: &Value) -> Result<Value> {
        let stream = UnixStream::connect(paths.socket()).await?;
        let (read, mut write) = stream.into_split();
        write.write_all(format!("{request}\n").as_bytes()).await?;
        let line = BufReader::new(read)
            .lines()
            .next_line()
            .await?
            .context("the node closed the connection")?;
        Ok(serde_json::from_str(&line)?)
    }

    pub fn remove(paths: &Paths) {
        let _ = std::fs::remove_file(paths.socket());
    }

    pub async fn shutdown_signal() {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("cannot listen for SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
}

/// Windows: a named pipe, `\\.\pipe\bricolo-<hash of the data
/// folder>` (see `bricolo_runtime::paths::pipe_name`), with the same JSON lines.
/// Its DACL lets only this user connect, and remote clients are refused.
#[cfg(windows)]
mod imp {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Context, Result, bail};
    use bricolo_runtime::Paths;
    use serde_json::Value;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    use super::Node;
    use crate::pipe_security::OwnerOnly;

    /// What the pipe answers to a client while every instance is in use.
    const ERROR_PIPE_BUSY: i32 = 231;

    pub struct Listener {
        name: String,
        security: OwnerOnly,
        first: NamedPipeServer,
    }

    fn create(name: &str, security: &OwnerOnly, first: bool) -> std::io::Result<NamedPipeServer> {
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        // SAFETY: `security` holds a valid SECURITY_ATTRIBUTES, and outlives
        // the call; the pipe copies the descriptor.
        unsafe { options.create_with_security_attributes_raw(name, security.as_ptr()) }
    }

    /// Creates the first pipe instance. Fails if a node already has it.
    pub async fn bind(paths: &Paths) -> Result<Listener> {
        let name = paths.socket().to_string_lossy().into_owned();
        let security = OwnerOnly::new().context("cannot build the pipe's security descriptor")?;
        let first = match create(&name, &security, true) {
            Ok(pipe) => pipe,
            Err(e) if ClientOptions::new().open(&name).is_ok() => {
                bail!("a node is already running on {name} ({e})")
            }
            Err(e) => return Err(e).with_context(|| format!("cannot create {name}")),
        };
        Ok(Listener {
            name,
            security,
            first,
        })
    }

    pub async fn serve(listener: Listener, node: Arc<Node>) {
        let Listener {
            name,
            security,
            first,
        } = listener;
        let mut waiting = first;
        loop {
            if let Err(e) = waiting.connect().await {
                tracing::warn!("control pipe: {e}");
            }
            // A new instance waits for the next client before this one is
            // handed over, so a client never finds the pipe missing.
            let next = loop {
                match create(&name, &security, false) {
                    Ok(pipe) => break pipe,
                    Err(e) => {
                        tracing::warn!("cannot create another {name}: {e}");
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            };
            let stream = std::mem::replace(&mut waiting, next);
            let node = node.clone();
            tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(stream);
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if matches!(serde_json::from_str(&line), Ok(super::Request::McpSession)) {
                        super::mcp_session(&node, lines.into_inner(), write).await;
                        break;
                    }
                    if matches!(serde_json::from_str(&line), Ok(super::Request::Subscribe)) {
                        tokio::select! {
                            _ = crate::subscribe::stream(&node, &mut write) => {}
                            _ = async { while let Ok(Some(_)) = lines.next_line().await {} } => {}
                        }
                        break;
                    }
                    let answer = super::handle(&node, &line).await;
                    let mut out = answer.to_string();
                    out.push('\n');
                    if write.write_all(out.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    }

    /// Opens the pipe, waiting up to 2 s while every instance is busy.
    pub(super) async fn open(
        name: &str,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
        let mut tries = 0;
        loop {
            match ClientOptions::new().open(name) {
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 40 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                other => return other,
            }
        }
    }

    pub async fn connect(paths: &Paths) -> Result<super::Conn> {
        let pipe = open(&paths.socket().to_string_lossy()).await?;
        let (read, write) = tokio::io::split(pipe);
        Ok((Box::new(read), Box::new(write)))
    }

    pub async fn request(paths: &Paths, request: &Value) -> Result<Value> {
        let pipe = open(&paths.socket().to_string_lossy()).await?;
        let (read, mut write) = tokio::io::split(pipe);
        write.write_all(format!("{request}\n").as_bytes()).await?;
        let line = BufReader::new(read)
            .lines()
            .next_line()
            .await?
            .context("the node closed the connection")?;
        Ok(serde_json::from_str(&line)?)
    }

    /// Nothing to remove: the pipe goes away with its last handle.
    pub fn remove(_: &Paths) {}

    /// Ctrl+C or Ctrl+Break in the console, the console closing, the user
    /// logging off or Windows shutting down. After a close, logoff or
    /// shutdown event Windows gives the process about 5 s.
    pub async fn shutdown_signal() {
        use tokio::signal::windows::{ctrl_break, ctrl_close, ctrl_logoff, ctrl_shutdown};
        let (Ok(mut brk), Ok(mut close), Ok(mut logoff), Ok(mut shutdown)) =
            (ctrl_break(), ctrl_close(), ctrl_logoff(), ctrl_shutdown())
        else {
            tracing::warn!("cannot listen for console events; only Ctrl+C stops the node");
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = brk.recv() => {}
            _ = close.recv() => {}
            _ = logoff.recv() => {}
            _ = shutdown.recv() => {}
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::sync::Arc;

    use anyhow::{Result, bail};
    use bricolo_runtime::Paths;
    use serde_json::Value;

    use super::Node;

    pub struct Listener;

    pub async fn bind(_: &Paths) -> Result<Listener> {
        bail!("the control socket needs Unix or Windows")
    }

    pub async fn serve(_: Listener, _: Arc<Node>) {}

    pub async fn request(_: &Paths, _: &Value) -> Result<Value> {
        bail!("the control socket needs Unix or Windows")
    }

    pub async fn connect(_: &Paths) -> Result<super::Conn> {
        bail!("the control socket needs Unix or Windows")
    }

    pub fn remove(_: &Paths) {}

    pub async fn shutdown_signal() {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_panes_for_missing_permissions() {
        let answer = |a: Option<bool>, p: Option<bool>, i: Option<bool>| json!({ "permissions": { "accessibility": a, "post_events": p, "input_monitoring": i } });
        assert_eq!(
            settings_panes(&answer(Some(true), Some(false), Some(false))),
            ["Privacy_Accessibility", "Privacy_ListenEvent"]
        );
        assert!(settings_panes(&answer(Some(true), Some(true), Some(true))).is_empty());
        assert!(settings_panes(&answer(None, None, None)).is_empty());
    }

    #[test]
    fn parses_requests() {
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"status"}"#).unwrap(),
            Request::Status
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"reload","plugin":"hello"}"#).unwrap(),
            Request::Reload {
                plugin: "hello".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"reload-script","script":"speak"}"#).unwrap(),
            Request::ReloadScript {
                script: "speak".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"reload-peers"}"#).unwrap(),
            Request::ReloadPeers
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"reload-sidecars"}"#).unwrap(),
            Request::ReloadSidecars
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"discover"}"#).unwrap(),
            Request::Discover
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"fetch-plugin","peer":"mac","sha256":"ab"}"#)
                .unwrap(),
            Request::FetchPlugin {
                peer: "mac".into(),
                sha256: "ab".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"permissions"}"#).unwrap(),
            Request::Permissions {
                request: false,
                grant: None
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(
                r#"{"cmd":"permissions","request":true,"grant":"camera"}"#
            )
            .unwrap(),
            Request::Permissions {
                request: true,
                grant: Some("camera".into())
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"library"}"#).unwrap(),
            Request::Library
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"open-settings","grant":"camera"}"#).unwrap(),
            Request::OpenSettings {
                grant: "camera".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(
                r#"{"cmd":"call","capability":"disk","method":"read","args":["~/a"]}"#
            )
            .unwrap(),
            Request::Call {
                capability: "disk".into(),
                method: "read".into(),
                args: vec![json!("~/a")],
                grants: vec![],
                client: None,
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"fire","plugin":"p","hotkey":"go"}"#)
                .unwrap(),
            Request::Fire {
                plugin: "p".into(),
                hotkey: "go".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"disable","plugin":"p"}"#).unwrap(),
            Request::Disable {
                plugin: Some("p".into()),
                script: None
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"restart-sidecar","sidecar":"stt"}"#)
                .unwrap(),
            Request::RestartSidecar {
                sidecar: "stt".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(
                r#"{"cmd":"ui-action","action":{"kind":"button","owner":"p","id":"go"}}"#
            )
            .unwrap(),
            Request::UiAction {
                action: UiAction::Button {
                    owner: "p".into(),
                    id: "go".into()
                }
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(
                r#"{"cmd":"set-hotkey","script":"s","id":"h1","chord":"cmd+k"}"#
            )
            .unwrap(),
            Request::SetHotkey {
                plugin: None,
                script: Some("s".into()),
                id: "h1".into(),
                chord: Some("cmd+k".into()),
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"set-hotkey","plugin":"p","id":"go"}"#)
                .unwrap(),
            Request::SetHotkey {
                plugin: Some("p".into()),
                script: None,
                id: "go".into(),
                chord: None,
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"subscribe"}"#).unwrap(),
            Request::Subscribe
        );
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"explode"}"#).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn settings_are_read_from_plugin_toml_and_written_to_node_toml() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(paths.plugin_dir("demo")).unwrap();
        std::fs::write(
            paths.plugin_dir("demo").join("plugin.toml"),
            "name = \"demo\"\nversion = \"1\"\n[settings]\nturns = 50\nvoice = \"alba\"\n\
             [settings_ui.voice]\nchoices = [\"alba\", \"marius\"]\n",
        )
        .unwrap();
        std::fs::write(paths.node_toml(), "# mine\nname = \"mac\"\n").unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();

        let answer = handle(&node, r#"{"cmd":"settings","plugin":"demo"}"#).await;
        assert_eq!(answer["settings"][0]["key"], "turns", "{answer}");
        assert_eq!(answer["settings"][1]["kind"], "choice", "{answer}");

        let answer = handle(
            &node,
            r#"{"cmd":"set-settings","plugin":"demo","values":{"voice":"marius"}}"#,
        )
        .await;
        assert_eq!(answer["settings"][1]["value"], "marius", "{answer}");
        assert_eq!(answer["settings"][1]["overridden"], true, "{answer}");
        let text = std::fs::read_to_string(paths.node_toml()).unwrap();
        assert!(
            text.starts_with("# mine") && text.contains("[plugins.demo]"),
            "{text}"
        );

        let answer = handle(
            &node,
            r#"{"cmd":"set-settings","plugin":"demo","values":{"voice":"bob"}}"#,
        )
        .await;
        assert!(
            answer["error"].as_str().unwrap().contains("one of"),
            "{answer}"
        );
        let answer = handle(&node, r#"{"cmd":"settings","plugin":"nope"}"#).await;
        assert!(
            answer["error"].as_str().unwrap().contains("no plugin"),
            "{answer}"
        );
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disabled_scripts_stay_off_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(paths.scripts_dir()).unwrap();
        std::fs::write(paths.scripts_dir().join("a.lua"), "bricolo.log.info('a')").unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let answer = handle(&node, r#"{"cmd":"disable","script":"a"}"#).await;
        assert_eq!(answer["status"]["state"], json!("disabled"), "{answer}");
        let answer = handle(&node, r#"{"cmd":"disable","plugin":"nope"}"#).await;
        assert!(
            answer["error"].as_str().unwrap().contains("no plugin"),
            "{answer}"
        );
        let answer = handle(&node, r#"{"cmd":"enable"}"#).await;
        assert!(answer["error"].is_string(), "{answer}");
        node.shutdown().await;
        let saved = NodeState::load(&paths.state_toml()).unwrap();
        assert_eq!(saved.disabled_scripts, ["a".to_string()].into());

        // A new node reads state.toml and keeps the script off.
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let status = handle(&node, r#"{"cmd":"status"}"#).await;
        assert_eq!(status["scripts"][0]["state"], json!("disabled"), "{status}");
        let answer = handle(&node, r#"{"cmd":"enable","script":"a"}"#).await;
        assert_eq!(answer["status"]["state"], json!("running"), "{answer}");
        assert!(
            NodeState::load(&paths.state_toml())
                .unwrap()
                .disabled_scripts
                .is_empty()
        );
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ui_actions_reach_a_script_and_sidecars_restart_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(paths.scripts_dir()).unwrap();
        std::fs::write(
            paths.scripts_dir().join("t.lua"),
            r#"bricolo.ui.set_tile{title = "T", buttons = {{id = "go"}},
                on_button = function(id) bricolo.log.info("pressed " .. id) end}"#,
        )
        .unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let request = json!({
            "cmd": "ui-action",
            "action": { "kind": "button", "owner": "t.lua", "id": "go" },
        });
        let answer = handle(&node, &request.to_string()).await;
        assert_eq!(answer, json!({ "ok": true }));
        let start = std::time::Instant::now();
        while !node
            .runtime
            .recent_logs(Some("t.lua"), 10)
            .iter()
            .any(|l| l.message == "pressed go")
        {
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let request = json!({
            "cmd": "ui-action",
            "action": { "kind": "button", "owner": "t.lua", "id": "nope" },
        });
        let answer = handle(&node, &request.to_string()).await;
        assert!(
            answer["error"].as_str().unwrap().contains("no button"),
            "{answer}"
        );
        let answer = handle(&node, r#"{"cmd":"restart-sidecar","sidecar":"x"}"#).await;
        assert!(
            answer["error"].as_str().unwrap().contains("no sidecar"),
            "{answer}"
        );
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reload_sidecars_applies_node_toml() {
        let Some(python) = ["/usr/bin/python3", "/opt/homebrew/bin/python3"]
            .into_iter()
            .find(|p| std::path::Path::new(p).is_file())
        else {
            eprintln!("python3 missing; skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(&paths.config).unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let server = format!(
            "import http.server\n\
             class H(http.server.BaseHTTPRequestHandler):\n\
             \x20   def do_GET(self):\n\
             \x20       self.send_response(200); self.end_headers(); self.wfile.write(b'{{\"ready\": true}}')\n\
             http.server.HTTPServer(('127.0.0.1', {port}), H).serve_forever()\n"
        );
        let mut doc = toml::Table::new();
        let mut sidecar = toml::Table::new();
        sidecar.insert("command".into(), python.into());
        sidecar.insert("args".into(), vec!["-c".to_string(), server].into());
        sidecar.insert("port".into(), i64::from(port).into());
        doc.insert(
            "sidecars".into(),
            toml::Table::from_iter([("echo".to_string(), sidecar.into())]).into(),
        );
        std::fs::write(paths.node_toml(), toml::to_string(&doc).unwrap()).unwrap();

        let answer = handle(&node, r#"{"cmd":"reload-sidecars"}"#).await;
        assert_eq!(answer["sidecars"]["added"], json!(["echo"]), "{answer}");
        let start = std::time::Instant::now();
        loop {
            let status = handle(&node, r#"{"cmd":"status"}"#).await;
            if status["sidecars"][0]["state"] == json!("ready") {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "{status}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        std::fs::write(paths.node_toml(), "").unwrap();
        let answer = handle(&node, r#"{"cmd":"reload-sidecars"}"#).await;
        assert_eq!(answer["sidecars"]["removed"], json!(["echo"]), "{answer}");
        let status = handle(&node, r#"{"cmd":"status"}"#).await;
        assert_eq!(status["sidecars"], json!([]));

        std::fs::write(paths.node_toml(), "[sidecars.x]\n").unwrap();
        let answer = handle(&node, r#"{"cmd":"reload-sidecars"}"#).await;
        assert!(answer["error"].is_string(), "{answer}");
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn status_reports_an_empty_node() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let answer = handle(&node, r#"{"cmd":"status"}"#).await;
        assert_eq!(answer["running"], json!(true));
        assert_eq!(answer["plugins"], json!([]));
        assert_eq!(answer["scripts"], json!([]));
        assert_eq!(answer["sidecars"], json!([]));
        assert_eq!(answer["active_sensors"], json!([]));
        assert_eq!(answer["mesh"], json!({ "enabled": false }));
        assert_eq!(answer["rules"], json!({ "timeout_secs": 30, "routes": {} }));
        let answer = handle(&node, r#"{"cmd":"fire","plugin":"nope","hotkey":"x"}"#).await;
        assert!(answer["error"].as_str().unwrap().contains("not running"));
        let answer = handle(&node, r#"{"cmd":"set-hotkey","script":"x","id":"h1"}"#).await;
        assert!(
            answer["error"].as_str().unwrap().contains("no hotkey"),
            "{answer}"
        );
        let answer = handle(&node, r#"{"cmd":"set-hotkey","id":"h1"}"#).await;
        assert!(answer["error"].is_string(), "{answer}");
        let answer = handle(&node, "nonsense").await;
        assert!(answer["error"].as_str().unwrap().starts_with("bad request"));
    }

    /// The socket (a named pipe on Windows) end to end: one request per
    /// connection, a subscription that streams, and a second node refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_control_socket_answers_requests_and_streams() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(&paths.data).unwrap();
        assert!(request(&paths, &json!({ "cmd": "status" })).await.is_err());
        let listener = imp::bind(&paths).await.unwrap();
        let node = Arc::new(Node::start(&paths, NodeOptions::default()).await.unwrap());
        let server = tokio::spawn(imp::serve(listener, node.clone()));

        for _ in 0..3 {
            let answer = request(&paths, &json!({ "cmd": "status" })).await.unwrap();
            assert_eq!(answer["running"], json!(true), "{answer}");
        }
        let err = imp::bind(&paths).await.err().unwrap();
        assert!(err.to_string().contains("already running"), "{err:#}");

        #[cfg(unix)]
        let stream = tokio::net::UnixStream::connect(paths.socket())
            .await
            .unwrap();
        #[cfg(windows)]
        let stream = imp::open(&paths.socket().to_string_lossy()).await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        write.write_all(b"{\"cmd\":\"subscribe\"}\n").await.unwrap();
        let mut lines = BufReader::new(read).lines();
        let first: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["event"], "status", "{first}");
        // Requests on other connections still get answers while it streams.
        let answer = request(&paths, &json!({ "cmd": "status" })).await.unwrap();
        assert_eq!(answer["running"], json!(true), "{answer}");

        server.abort();
        node.shutdown().await;
        imp::remove(&paths);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn status_lists_scripts_and_their_timer_events_arrive() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(paths.scripts_dir()).unwrap();
        std::fs::write(
            paths.scripts_dir().join("tick.lua"),
            "bricolo.timer.after(10, function() bricolo.log.info('fired') end)",
        )
        .unwrap();
        std::fs::write(paths.scripts_dir().join("broken.lua"), "nope").unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let answer = handle(&node, r#"{"cmd":"status"}"#).await;
        let scripts = answer["scripts"].as_array().unwrap();
        assert_eq!(scripts.len(), 2, "{answer}");
        assert_eq!(scripts[0]["name"], json!("broken"));
        assert_eq!(scripts[0]["state"], json!("off"));
        assert_eq!(scripts[1]["name"], json!("tick"));
        assert_eq!(scripts[1]["state"], json!("running"));
        // The runtime's provider sink hands script timers to the Lua tier.
        let start = std::time::Instant::now();
        while !node
            .runtime
            .recent_logs(Some("tick.lua"), 10)
            .iter()
            .any(|l| l.message == "fired")
        {
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn library_lists_the_stdlib_scripts_and_grants() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        std::fs::create_dir_all(paths.scripts_dir()).unwrap();
        std::fs::write(paths.scripts_dir().join("speak.lua"), "-- nothing").unwrap();
        // A plugin approved once but no longer installed is left out.
        std::fs::create_dir_all(paths.permissions_toml().parent().unwrap()).unwrap();
        std::fs::write(paths.permissions_toml(), "[gone]\ngranted = [\"mic\"]\n").unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let answer = handle(&node, r#"{"cmd":"library"}"#).await;
        assert_eq!(answer["scripts"], json!(["speak"]), "{answer}");
        let caps = answer["capabilities"].as_array().unwrap();
        assert_eq!(caps.len(), bricolo_providers::catalog::capabilities().len());
        let mic = caps.iter().find(|c| c["name"] == "mic").unwrap();
        assert_eq!(mic["plugins"], json!([]));
        assert_eq!(mic["needs"], json!("microphone"));
        assert_eq!(answer["grants"].as_array().unwrap().len(), 6);
        assert!(answer["holder"].is_string());
        let answer = handle(&node, r#"{"cmd":"open-settings","grant":"nope"}"#).await;
        assert!(answer["error"].is_string(), "{answer}");
        let answer = handle(
            &node,
            r#"{"cmd":"permissions","request":true,"grant":"nope"}"#,
        )
        .await;
        assert!(answer["error"].is_string(), "{answer}");
        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn call_runs_a_routed_capability() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "hi").unwrap();
        let node = Node::start(&paths, NodeOptions::default()).await.unwrap();
        let request = json!({
            "cmd": "call",
            "capability": "disk",
            "method": "read",
            "args": [root.join("a.txt")],
            "grants": [format!("disk:read:{}", root.display())],
        });
        let answer = handle(&node, &request.to_string()).await;
        assert_eq!(answer["ok"], json!([104, 105]), "{answer}");
        assert_eq!(answer["text"], json!("hi"));
        // No `[speech]` server in node.toml.
        let request = json!({ "cmd": "call", "capability": "speech", "method": "transcribe", "args": [[], "wav"] });
        let answer = handle(&node, &request.to_string()).await;
        assert_eq!(answer["call_error"], json!("unavailable"), "{answer}");
        let request = json!({ "cmd": "call", "capability": "disk", "method": "read", "args": ["/etc/hosts"] });
        let answer = handle(&node, &request.to_string()).await;
        assert_eq!(answer["call_error"], json!("denied"), "{answer}");
        node.shutdown().await;
    }
}
