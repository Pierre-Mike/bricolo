//! The MCP gateway inside `bricolo run`. It serves this node and every peer
//! that lets this node call it, over the control socket (`bricolo mcp` pipes
//! stdio to it) and, when `[mcp] listen` is set, over HTTP for agents that
//! reach it through a tunnel. Every node runs one, so an agent connected
//! to any of them sees the whole mesh.
//!
//! ```toml
//! [mcp]
//! enabled = true              # default; false turns the gateway off
//! listen = "127.0.0.1:8790"   # HTTP; no HTTP when missing
//! allow = ["*"]               # node.capability.method patterns
//! deny = ["*.disk.write"]
//! grants = ["disk:~/Notes"]   # disk grants for calls on this node
//! shell = ["/usr/bin/xterm"]  # programs virtual-screen.launch may start
//! origins = []                # browser origins allowed over HTTP
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use bricolo_mcp::http::Access;
use bricolo_mcp::{Backend, CallFailure, Catalog, NodeInfo, Policy, Server};
use bricolo_mesh::capabilities::{METHODS, dispatch};
use bricolo_mesh::wire::Json;
use bricolo_mesh::{LinkState, Mesh};
use bricolo_providers::disk::DiskGrant;
use bricolo_providers::{CallError, Caller};
use bricolo_runtime::paths::home_dir;
use bricolo_runtime::{Paths, Runtime};
use serde::Deserialize;
use serde_json::{Value, json};

/// How often the gateway checks whether its tool list changed.
const WATCH: Duration = Duration::from_secs(2);

/// How long a peer's method list stays fresh.
const PEER_TTL: Duration = Duration::from_secs(60);

/// The caller name a gateway call runs as, on this node and on peers.
const CALLER: &str = "bricolo-mcp";

/// `[mcp]` in node.toml.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub listen: Option<SocketAddr>,
    #[serde(default = "everything")]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub grants: Vec<String>,
    /// Absolute program paths `virtual-screen.launch` may start for an
    /// agent on this node, like a plugin's `[shell] allow`.
    #[serde(default)]
    pub shell: Vec<String>,
    #[serde(default)]
    pub origins: Vec<String>,
}

fn yes() -> bool {
    true
}

fn everything() -> Vec<String> {
    vec!["*".into()]
}

impl Default for McpConfig {
    fn default() -> Self {
        toml::from_str("").expect("defaults parse")
    }
}

impl McpConfig {
    pub fn from_table(table: &toml::Table) -> Result<Self> {
        table.clone().try_into().context("bad [mcp] in node.toml")
    }
}

/// Where the HTTP gateway's token lives.
pub fn token_path(paths: &Paths) -> PathBuf {
    paths.data.join("mcp.token")
}

/// The running gateway.
pub struct Gateway {
    pub server: Arc<Server>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// The HTTP address, once bound.
    pub http: Option<SocketAddr>,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Starts the gateway, or answers `None` when `[mcp] enabled = false`.
pub async fn start(
    paths: &Paths,
    node_name: &str,
    config: McpConfig,
    runtime: Arc<Runtime>,
    mesh: Option<Mesh>,
) -> Result<Option<Gateway>> {
    if !config.enabled {
        return Ok(None);
    }
    let home = home_dir().unwrap_or_default();
    let mut grants = Vec::new();
    for g in &config.grants {
        match DiskGrant::parse(g, &home) {
            Some(grant) => grants.push(grant),
            None => bail!("bad grant {g:?} in [mcp]; use disk:<path> or disk:read:<path>"),
        }
    }
    for p in &config.shell {
        if !bricolo_providers::shell::valid_program(p) {
            bail!("bad program {p:?} in [mcp] shell; use an absolute path");
        }
    }
    let shell = bricolo_providers::shell::ShellGrant {
        programs: config.shell.clone(),
        env: Vec::new(),
    };
    let backend = Arc::new(NodeBackend {
        name: node_name.to_string(),
        runtime,
        mesh,
        grants,
        shell,
        peers: Mutex::default(),
    });
    let catalog = Catalog::stdlib().map_err(anyhow::Error::msg)?;
    let policy = Policy::new(config.allow, config.deny);
    let server = Arc::new(Server::new(
        catalog,
        backend,
        policy,
        env!("CARGO_PKG_VERSION"),
    ));
    let mut tasks = vec![server.watch(WATCH)];
    let mut http = None;
    if let Some(addr) = config.listen {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot bind the MCP gateway to {addr}"))?;
        let bound = listener.local_addr()?;
        let token = bricolo_mcp::http::load_or_create_token(&token_path(paths))
            .context("cannot read or write the MCP token")?;
        let access = Access {
            token,
            origins: config.origins,
        };
        let s = server.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = bricolo_mcp::http::serve(s, listener, access).await {
                tracing::error!("the MCP HTTP gateway stopped: {e}");
            }
        }));
        tracing::info!(%bound, "MCP gateway listening over HTTP");
        http = Some(bound);
    }
    Ok(Some(Gateway {
        server,
        tasks,
        http,
    }))
}

/// `(capability, method)` pairs, as a node reports them.
type Methods = Vec<(String, String)>;

struct NodeBackend {
    name: String,
    runtime: Arc<Runtime>,
    mesh: Option<Mesh>,
    grants: Vec<DiskGrant>,
    shell: bricolo_providers::shell::ShellGrant,
    /// Each peer's method list and when it was fetched.
    peers: Mutex<HashMap<String, (Instant, Methods)>>,
}

fn pairs(methods: &[(&str, &str)]) -> Vec<(String, String)> {
    methods
        .iter()
        .map(|(c, m)| (c.to_string(), m.to_string()))
        .collect()
}

fn failure(e: CallError) -> CallFailure {
    let kind = match &e {
        CallError::Unavailable => "unavailable",
        CallError::Timeout => "timeout",
        CallError::Denied => "denied",
        CallError::Failed(_) => "failed",
    };
    CallFailure {
        kind: kind.into(),
        message: e.to_string(),
    }
}

impl NodeBackend {
    /// The peer's methods: from the cache while fresh, else asked again.
    /// A peer that cannot answer keeps its old list.
    async fn peer_methods(&self, mesh: &Mesh, peer: &str) -> Vec<(String, String)> {
        let cached = self.peers.lock().unwrap().get(peer).cloned();
        if let Some((at, methods)) = &cached
            && at.elapsed() < PEER_TTL
        {
            return methods.clone();
        }
        match tokio::time::timeout(Duration::from_secs(5), mesh.peer_methods(peer)).await {
            Ok(Ok(methods)) => {
                self.peers
                    .lock()
                    .unwrap()
                    .insert(peer.to_string(), (Instant::now(), methods.clone()));
                methods
            }
            other => {
                tracing::debug!(peer, ?other, "cannot list the peer's methods");
                cached.map(|(_, m)| m).unwrap_or_default()
            }
        }
    }
}

#[async_trait]
impl Backend for NodeBackend {
    async fn nodes(&self) -> Vec<NodeInfo> {
        let mut out = vec![NodeInfo {
            name: self.name.clone(),
            online: true,
            local: true,
            methods: pairs(METHODS),
            details: json!({ "id": self.mesh.as_ref().map(|m| m.id().to_string()) }),
        }];
        let Some(mesh) = &self.mesh else { return out };
        for peer in mesh.status().peers {
            let online = peer.state == LinkState::Connected;
            let methods = if online {
                self.peer_methods(mesh, &peer.name).await
            } else {
                // Forget it, so the list is fetched fresh when it returns.
                self.peers.lock().unwrap().remove(&peer.name);
                Vec::new()
            };
            out.push(NodeInfo {
                name: peer.name,
                online,
                local: false,
                methods,
                details: json!({
                    "id": peer.id,
                    "state": peer.state,
                    "via": peer.via,
                    "last_seen": peer.last_seen,
                    "last_error": peer.last_error,
                }),
            });
        }
        out
    }

    async fn call(
        &self,
        node: &str,
        capability: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, CallFailure> {
        let args = serde_json::to_vec(&args).unwrap_or_default();
        let bytes = if node == self.name {
            let caller = Caller {
                plugin: CALLER.into(),
                disk: self.grants.clone(),
                shell: self.shell.clone(),
                ..Caller::default()
            };
            dispatch::<Json>(self.runtime.providers(), &caller, capability, method, &args).await
        } else {
            let Some(mesh) = &self.mesh else {
                return Err(failure(CallError::Unavailable));
            };
            // The peer applies the grants it gave this node.
            let caller = Caller {
                plugin: CALLER.into(),
                ..Caller::default()
            };
            mesh.call_json(node, capability, method, &caller, &args)
                .await
        }
        .map_err(failure)?;
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_parses() {
        let c = McpConfig::default();
        assert!(c.enabled);
        assert_eq!(c.allow, ["*"]);
        assert!(c.listen.is_none());

        let table: toml::Table = toml::from_str(
            r#"
            listen = "127.0.0.1:8790"
            deny = ["*.disk.write"]
            grants = ["disk:~/Notes"]
            shell = ["/usr/bin/xterm"]
            "#,
        )
        .unwrap();
        let c = McpConfig::from_table(&table).unwrap();
        assert_eq!(c.listen, Some("127.0.0.1:8790".parse().unwrap()));
        assert_eq!(c.deny, ["*.disk.write"]);
        assert_eq!(c.shell, ["/usr/bin/xterm"]);

        let bad: toml::Table = toml::from_str("listn = 1").unwrap();
        assert!(McpConfig::from_table(&bad).is_err());
    }
}
