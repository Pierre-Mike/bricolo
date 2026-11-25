//! `bricolo id`, `bricolo peer`, and the mesh that `bricolo run` starts.
//!
//! `node.toml`:
//!
//! ```toml
//! [mesh]
//! enabled = true            # start the mesh with `bricolo run`
//! transports = ["iroh"]     # "iroh", "tailscale", or both, in order of preference
//! relays = true             # iroh: n0 relays and DNS discovery; false: direct addresses only
//! port = 0                  # iroh: UDP port; 0 picks a free one. Fix it when peers
//!                           # list this node in their `addrs`.
//!
//! [mesh.tailscale]
//! port = 7787               # UDP port on the tailnet address, the same on every node
//! command = "tailscale"     # the CLI that answers `status --json`
//! ```

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bricolo_mesh::transport::tailscale::DEFAULT_PORT;
use bricolo_mesh::{
    EndpointId, IrohConfig, Mesh, MeshConfig, PeerList, StatusSource, TailscaleConfig,
    TransportConfig, TransportKind, identity, peers,
};
use bricolo_runtime::Paths;
use bricolo_runtime::node_config::NodeConfig;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "iroh_only")]
    pub transports: Vec<String>,
    #[serde(default = "yes")]
    pub relays: bool,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub tailscale: TailscaleSettings,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TailscaleSettings {
    #[serde(default = "tailscale_port")]
    pub port: u16,
    #[serde(default)]
    pub command: Option<PathBuf>,
}

impl Default for TailscaleSettings {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            command: None,
        }
    }
}

fn yes() -> bool {
    true
}

fn iroh_only() -> Vec<String> {
    vec!["iroh".into()]
}

fn tailscale_port() -> u16 {
    DEFAULT_PORT
}

impl Settings {
    pub fn from_node(node: &NodeConfig) -> Result<Self> {
        node.mesh
            .clone()
            .try_into()
            .context("invalid [mesh] in node.toml")
    }

    /// The transports, in the order `transports` lists them.
    pub fn kinds(&self) -> Result<Vec<TransportKind>> {
        if self.transports.is_empty() {
            bail!("[mesh] transports is empty; list \"iroh\", \"tailscale\" or both");
        }
        let mut kinds = Vec::new();
        for name in &self.transports {
            let kind = TransportKind::parse(name).with_context(|| {
                format!("[mesh] transports: {name:?} is not \"iroh\" or \"tailscale\"")
            })?;
            if kinds.contains(&kind) {
                bail!("[mesh] transports lists {name} twice");
            }
            kinds.push(kind);
        }
        Ok(kinds)
    }

    fn config(&self, call_timeout: Duration) -> Result<MeshConfig> {
        let bind = if self.port == 0 {
            Vec::new()
        } else {
            vec![
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, self.port)),
                SocketAddr::from((Ipv6Addr::UNSPECIFIED, self.port)),
            ]
        };
        let transports = self
            .kinds()?
            .into_iter()
            .map(|kind| match kind {
                TransportKind::Iroh => TransportConfig::Iroh(IrohConfig {
                    relays: self.relays,
                    bind: bind.clone(),
                }),
                TransportKind::Tailscale => TransportConfig::Tailscale(TailscaleConfig {
                    port: self.tailscale.port,
                    status: match &self.tailscale.command {
                        Some(cli) => StatusSource::Command(cli.clone()),
                        None => StatusSource::default_command(),
                    },
                    bind: Vec::new(),
                    poll: bricolo_mesh::transport::tailscale::DEFAULT_POLL,
                }),
            })
            .collect();
        Ok(MeshConfig {
            transports,
            call_timeout,
        })
    }
}

/// Starts the mesh unless `[mesh] enabled = false`.
pub async fn start(paths: &Paths, call_timeout: Duration) -> Result<Option<Mesh>> {
    let node = NodeConfig::load(&paths.node_toml())?;
    let settings = Settings::from_node(&node)?;
    if !settings.enabled {
        tracing::info!("mesh disabled in node.toml");
        return Ok(None);
    }
    let key = identity::load_or_create(&paths.node_key())?;
    let peers = PeerList::load(&paths.peers_toml())?;
    let mesh = Mesh::start(key, peers, settings.config(call_timeout)?).await?;
    Ok(Some(mesh))
}

/// `bricolo id`: this node's endpoint id and the lines a peer needs.
pub fn print_id(paths: &Paths, name: Option<String>) -> Result<()> {
    let key = identity::load_or_create(&paths.node_key())?;
    let node = NodeConfig::load(&paths.node_toml())?;
    let settings = Settings::from_node(&node)?;
    let id = key.public();
    let name = name.unwrap_or_else(|| match node.name.as_str() {
        "local" => "my-device".to_string(),
        n => n.to_string(),
    });
    println!("{id}");
    println!();
    println!("On the other device, trust this one with:");
    println!();
    println!("  bricolo peer add {name} {id} --allow tts,speech");
    println!();
    println!("or paste this into its peers.toml and edit `allow`:");
    println!();
    print!("{}", peers::toml_block(&name, &id, &[], &[]));
    let kinds = settings.kinds()?;
    if kinds.contains(&TransportKind::Iroh) && settings.port != 0 {
        println!(
            "# addrs = [\"<this device's IP>:{}\"]  # optional, skips discovery",
            settings.port
        );
    }
    if kinds.contains(&TransportKind::Tailscale) {
        println!(
            "# tailnet = \"<this device's tailnet machine name>\"  # when it differs from {name:?}"
        );
    }
    Ok(())
}

/// `bricolo peer add`.
pub fn add_peer(
    paths: &Paths,
    name: &str,
    id: &str,
    allow: &[String],
    addrs: &[SocketAddr],
) -> Result<()> {
    let id: EndpointId = id
        .trim()
        .parse()
        .with_context(|| format!("{id:?} is not an endpoint id; copy it from `bricolo id`"))?;
    let path = paths.peers_toml();
    peers::append(&path, name, &id, allow, addrs)?;
    let what = if allow.is_empty() {
        "nothing yet".to_string()
    } else {
        allow.join(", ")
    };
    println!("added {name} to {}; it may call: {what}", path.display());
    Ok(())
}

/// `bricolo peer discover`: what each transport of the running node sees.
pub async fn discover(paths: &Paths) -> Result<std::process::ExitCode> {
    let answer = crate::control::request(paths, &serde_json::json!({ "cmd": "discover" }))
        .await
        .with_context(|| {
            format!(
                "no node answers on {}; start bricolo run",
                paths.socket().display()
            )
        })?;
    if let Some(e) = answer["error"].as_str() {
        bail!("{e}");
    }
    let known = PeerList::load(&paths.peers_toml())?;
    for t in answer["transports"].as_array().into_iter().flatten() {
        let kind = t["transport"].as_str().unwrap_or("?");
        println!("{kind}:");
        if let Some(e) = t["error"].as_str() {
            println!("  error: {e}");
            continue;
        }
        let peers = t["peers"].as_array().cloned().unwrap_or_default();
        if peers.is_empty() {
            println!("  nothing");
        }
        for p in peers {
            let id = &p["id"];
            let label = id["host"]
                .as_str()
                .or(id["key"].as_str())
                .unwrap_or("?")
                .to_string();
            let online = match p["online"].as_bool() {
                Some(true) => "online",
                Some(false) => "offline",
                None => "",
            };
            let trusted = known.peers.values().find(|k| match id["host"].as_str() {
                Some(host) => {
                    let wanted = k.tailnet.as_deref().unwrap_or(&k.name);
                    wanted.split(':').next() == Some(host)
                }
                None => id["key"].as_str() == Some(k.id.to_string().as_str()),
            });
            let trusted = trusted
                .map(|k| format!("peer {}", k.name))
                .unwrap_or_default();
            let addrs: Vec<&str> = p["addrs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str())
                .collect();
            println!(
                "  {label:<24} {online:<8} {trusted:<16} {}",
                addrs.join(", ")
            );
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// `bricolo peer list`: the peers in peers.toml, with their state when a node
/// is running (`status` is the node's `mesh` status).
pub fn list_peers(paths: &Paths, status: Option<&Value>) -> Result<()> {
    let list = PeerList::load(&paths.peers_toml())?;
    if list.peers.is_empty() {
        println!("no peers in {}", paths.peers_toml().display());
        return Ok(());
    }
    let live = |name: &str| -> Option<&Value> {
        status?["peers"]
            .as_array()?
            .iter()
            .find(|p| p["name"] == name)
    };
    println!("{:<16} {:<22} {:<18} ALLOW", "NAME", "STATE", "ID");
    for peer in list.peers.values() {
        let state = match live(&peer.name) {
            Some(p) => match (p["state"].as_str(), p["via"].as_str()) {
                (Some(state), Some(via)) => format!("{state} ({via})"),
                (Some(state), None) => state.to_string(),
                _ => "?".into(),
            },
            None if status.is_some() => "not loaded".into(),
            None => "node off".into(),
        };
        let id = peer.id.to_string();
        println!(
            "{:<16} {:<22} {:<18} {}",
            peer.name,
            state,
            format!("{}…", &id[..16]),
            peer.allow.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_relays_and_any_port() {
        let node: NodeConfig = toml::from_str("").unwrap();
        let s = Settings::from_node(&node).unwrap();
        assert_eq!(
            s,
            Settings {
                enabled: true,
                transports: vec!["iroh".into()],
                relays: true,
                port: 0,
                tailscale: TailscaleSettings::default(),
            }
        );
        let config = s.config(Duration::from_secs(1)).unwrap();
        assert_eq!(
            config.transports,
            [TransportConfig::Iroh(IrohConfig::default())]
        );
        let node: NodeConfig = toml::from_str("[mesh]\nrelays = false\nport = 7777").unwrap();
        let config = Settings::from_node(&node)
            .unwrap()
            .config(Duration::from_secs(1))
            .unwrap();
        let TransportConfig::Iroh(iroh) = &config.transports[0] else {
            panic!("{config:?}")
        };
        assert!(!iroh.relays);
        assert_eq!(iroh.bind[0].port(), 7777);
        let node: NodeConfig = toml::from_str("[mesh]\ncolour = 1").unwrap();
        assert!(Settings::from_node(&node).is_err());
    }

    #[test]
    fn transports_are_chosen_in_node_toml() {
        let config = |text: &str| {
            let node: NodeConfig = toml::from_str(text).unwrap();
            Settings::from_node(&node)?.config(Duration::from_secs(1))
        };
        let only = config("[mesh]\ntransports = [\"tailscale\"]\n[mesh.tailscale]\nport = 9000\ncommand = \"/opt/ts\"").unwrap();
        assert_eq!(
            only.transports,
            [TransportConfig::Tailscale(TailscaleConfig {
                port: 9000,
                status: StatusSource::Command("/opt/ts".into()),
                bind: Vec::new(),
                poll: bricolo_mesh::transport::tailscale::DEFAULT_POLL,
            })]
        );
        let both = config("[mesh]\ntransports = [\"tailscale\", \"iroh\"]").unwrap();
        let kinds: Vec<_> = both.transports.iter().map(|t| t.kind()).collect();
        assert_eq!(kinds, [TransportKind::Tailscale, TransportKind::Iroh]);
        let TransportConfig::Tailscale(ts) = &both.transports[0] else {
            panic!("{both:?}")
        };
        assert_eq!(ts.port, DEFAULT_PORT);
        for bad in [
            "[mesh]\ntransports = []",
            "[mesh]\ntransports = [\"wifi\"]",
            "[mesh]\ntransports = [\"iroh\", \"iroh\"]",
            "[mesh.tailscale]\ncolour = 1",
        ] {
            assert!(config(bad).is_err(), "accepted {bad}");
        }
    }
}
