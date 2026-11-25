//! `peers.toml`: the nodes this node trusts, and what each may call here.
//!
//! ```toml
//! [peers.desktop]
//! key = "<endpoint id from `bricolo id` on the desktop>"
//! # What the desktop may call on this node. `disk` needs a path, like a
//! # plugin grant: "disk:~/Shared" (read-write) or "disk:read:~/Shared".
//! allow = ["tts", "speech", "disk:read:~/Shared"]
//! # Optional direct addresses to try before discovery and relays.
//! addrs = ["192.168.1.20:7777"]
//! # Optional machine name on the tailnet, for the Tailscale transport.
//! # Defaults to the peer's name here.
//! tailnet = "desktop"
//! ```
//!
//! A key that is not listed here is refused during the handshake.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use bricolo_providers::disk::DiskGrant;
use iroh::EndpointId;
use serde::Deserialize;

use crate::error::MeshError;

/// The target name rules.toml uses for this node; no peer may take it.
const LOCAL: &str = "local";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    peers: BTreeMap<String, Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    key: String,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    addrs: Vec<SocketAddr>,
    #[serde(default)]
    tailnet: Option<String>,
}

/// One trusted peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub id: EndpointId,
    pub allow: Vec<String>,
    pub addrs: Vec<SocketAddr>,
    /// The peer's tailnet machine, `host` or `host:port`. `None` means
    /// the Tailscale transport looks for a machine called `name`.
    pub tailnet: Option<String>,
}

impl Peer {
    pub fn new(name: &str, id: EndpointId, allow: &[&str]) -> Self {
        Self {
            name: name.to_string(),
            id,
            allow: allow.iter().map(|s| s.to_string()).collect(),
            addrs: Vec::new(),
            tailnet: None,
        }
    }

    /// True when this peer may call `capability` on this node. For `disk`,
    /// any `disk:` grant counts; the grants then limit the paths.
    pub fn allows(&self, capability: &str) -> bool {
        self.allow.iter().any(|a| {
            a == capability || (capability == "disk" && DiskGrant::parse(a, probe_home()).is_some())
        })
    }

    /// The `disk:` grants in the allow list, with `~/` expanded to `home`.
    pub fn disk_grants(&self, home: &Path) -> Vec<DiskGrant> {
        self.allow
            .iter()
            .filter_map(|a| DiskGrant::parse(a, home))
            .collect()
    }
}

/// Every peer in `peers.toml`, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerList {
    pub peers: BTreeMap<String, Peer>,
}

impl PeerList {
    pub fn parse(text: &str) -> Result<Self, MeshError> {
        let file: File = toml::from_str(text).map_err(|e| MeshError::peers(e.to_string()))?;
        let mut peers = BTreeMap::new();
        for (name, entry) in file.peers {
            check_name(&name)?;
            let id: EndpointId = entry
                .key
                .parse()
                .map_err(|e| MeshError::peers(format!("{name}: bad key {:?}: {e}", entry.key)))?;
            for a in &entry.allow {
                check_allow(&name, a)?;
            }
            if let Some(t) = &entry.tailnet {
                crate::transport::tailscale::parse_tailnet(t)
                    .map_err(|e| MeshError::peers(format!("{name}: {e}")))?;
            }
            if let Some(other) = peers.values().find(|p: &&Peer| p.id == id) {
                return Err(MeshError::peers(format!(
                    "{name} and {} have the same key",
                    other.name
                )));
            }
            peers.insert(
                name.clone(),
                Peer {
                    name,
                    id,
                    allow: entry.allow,
                    addrs: entry.addrs,
                    tailnet: entry.tailnet,
                },
            );
        }
        Ok(Self { peers })
    }

    /// Reads `path`; a missing file means no peers.
    pub fn load(path: &Path) -> Result<Self, MeshError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(MeshError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn by_id(&self, id: &EndpointId) -> Option<&Peer> {
        self.peers.values().find(|p| &p.id == id)
    }

    pub fn insert(&mut self, peer: Peer) {
        self.peers.insert(peer.name.clone(), peer);
    }
}

fn check_name(name: &str) -> Result<(), MeshError> {
    if name == LOCAL {
        return Err(MeshError::peers(
            "\"local\" names this node in rules.toml; pick another peer name",
        ));
    }
    let ok = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        return Err(MeshError::peers(format!(
            "peer name {name:?} may use only letters, digits, - and _"
        )));
    }
    Ok(())
}

/// A stand-in home folder to check that `~/` grants expand to an absolute
/// path; on Windows a path needs a drive for that.
fn probe_home() -> &'static Path {
    Path::new(if cfg!(windows) { "C:\\" } else { "/" })
}

fn check_allow(peer: &str, entry: &str) -> Result<(), MeshError> {
    if entry == "disk" || entry == "disk:read" {
        return Err(MeshError::peers(format!(
            "{peer}: disk needs a path, like \"disk:~/Shared\" or \"disk:read:~/Shared\""
        )));
    }
    if entry.starts_with("disk:") {
        return match DiskGrant::parse(entry, probe_home()) {
            Some(_) => Ok(()),
            None => Err(MeshError::peers(format!(
                "{peer}: {entry:?} needs an absolute path or one under ~/"
            ))),
        };
    }
    let ok = !entry.is_empty()
        && entry
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(MeshError::peers(format!(
            "{peer}: {entry:?} is not a capability name"
        )))
    }
}

/// The `[peers.<name>]` block for a peer, as `bricolo id` prints it and
/// `bricolo peer add` appends it.
pub fn toml_block(name: &str, id: &EndpointId, allow: &[String], addrs: &[SocketAddr]) -> String {
    let list = |items: Vec<String>| {
        let quoted: Vec<String> = items.iter().map(|s| format!("{s:?}")).collect();
        format!("[{}]", quoted.join(", "))
    };
    let mut block = format!(
        "[peers.{name}]\nkey = \"{id}\"\nallow = {}\n",
        list(allow.to_vec())
    );
    if !addrs.is_empty() {
        block.push_str(&format!(
            "addrs = {}\n",
            list(addrs.iter().map(|a| a.to_string()).collect())
        ));
    }
    block
}

/// Appends a new peer to `path`, keeping the file's comments. Refuses a
/// name or key that is already listed.
pub fn append(
    path: &PathBuf,
    name: &str,
    id: &EndpointId,
    allow: &[String],
    addrs: &[SocketAddr],
) -> Result<PeerList, MeshError> {
    let old = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(MeshError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let current = PeerList::parse(&old)?;
    if current.peers.contains_key(name) {
        return Err(MeshError::peers(format!(
            "{name} is already listed; edit {} to change it",
            path.display()
        )));
    }
    if let Some(p) = current.by_id(id) {
        return Err(MeshError::peers(format!(
            "that key is already listed as {}",
            p.name
        )));
    }
    let mut text = old;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&toml_block(name, id, allow, addrs));
    let list = PeerList::parse(&text)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|source| MeshError::Write {
            path: dir.display().to_string(),
            source,
        })?;
    }
    std::fs::write(path, text).map_err(|source| MeshError::Write {
        path: path.display().to_string(),
        source,
    })?;
    Ok(list)
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;

    fn key() -> EndpointId {
        SecretKey::generate().public()
    }

    #[test]
    fn parses_peers_and_checks_allow_lists() {
        let id = key();
        let list = PeerList::parse(&format!(
            "[peers.desktop]\nkey = \"{id}\"\nallow = [\"tts\", \"disk:read:~/Shared\"]\naddrs = [\"10.0.0.2:7777\"]\n"
        ))
        .unwrap();
        let p = &list.peers["desktop"];
        assert_eq!(p.id, id);
        assert!(p.allows("tts"));
        assert!(p.allows("disk"));
        assert!(!p.allows("speech"));
        assert_eq!(p.addrs, ["10.0.0.2:7777".parse().unwrap()]);
        assert_eq!(p.tailnet, None);
        let home = Path::new(if cfg!(windows) {
            "C:/home/me"
        } else {
            "/home/me"
        });
        let grants = p.disk_grants(home);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].root, home.join("Shared"));
        assert!(!grants[0].write);
        assert_eq!(list.by_id(&id).unwrap().name, "desktop");
    }

    #[test]
    fn rejects_bad_entries() {
        let id = key();
        let bad = [
            format!("[peers.local]\nkey = \"{id}\""),
            format!("[peers.\"a b\"]\nkey = \"{id}\""),
            "[peers.x]\nkey = \"nope\"".to_string(),
            format!("[peers.x]\nkey = \"{id}\"\nallow = [\"disk\"]"),
            format!("[peers.x]\nkey = \"{id}\"\nallow = [\"disk:relative\"]"),
            format!("[peers.x]\nkey = \"{id}\"\nallow = [\"TTS\"]"),
            format!("[peers.x]\nkey = \"{id}\"\ncolour = 1"),
            format!("[peers.x]\nkey = \"{id}\"\ntailnet = \"a b\""),
            format!("[peers.x]\nkey = \"{id}\"\n[peers.y]\nkey = \"{id}\""),
        ];
        for text in bad {
            assert!(PeerList::parse(&text).is_err(), "accepted: {text}");
        }
        assert_eq!(PeerList::parse("").unwrap(), PeerList::default());
    }

    #[test]
    fn missing_file_is_no_peers() {
        let list = PeerList::load(Path::new("/nonexistent/peers.toml")).unwrap();
        assert!(list.peers.is_empty());
    }

    #[test]
    fn append_keeps_comments_and_refuses_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.toml");
        std::fs::write(&path, "# my devices\n").unwrap();
        let (a, b) = (key(), key());
        append(&path, "desktop", &a, &["tts".into()], &[]).unwrap();
        let list = append(
            &path,
            "laptop",
            &b,
            &["speech".into()],
            &["127.0.0.1:7777".parse().unwrap()],
        )
        .unwrap();
        assert_eq!(list.peers.len(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my devices\n"), "{text}");
        assert!(append(&path, "desktop", &key(), &[], &[]).is_err());
        assert!(append(&path, "other", &a, &[], &[]).is_err());
        assert!(append(&path, "x", &key(), &["disk".into()], &[]).is_err());
        assert_eq!(PeerList::load(&path).unwrap(), list);
    }
}
