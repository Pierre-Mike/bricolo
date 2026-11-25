//! Where the node keeps its files. See "Files on disk" in docs/design.md.

use std::path::{Path, PathBuf};

pub const CONFIG_ENV: &str = "BRICOLO_CONFIG_DIR";
pub const DATA_ENV: &str = "BRICOLO_DATA_DIR";

#[derive(Clone, Debug)]
pub struct Paths {
    /// What you edit: `~/.config/bricolo`.
    pub config: PathBuf,
    /// What the node manages: `~/.local/share/bricolo`.
    pub data: PathBuf,
}

impl Paths {
    pub fn new(config: impl Into<PathBuf>, data: impl Into<PathBuf>) -> Self {
        Self {
            config: config.into(),
            data: data.into(),
        }
    }

    /// `$BRICOLO_CONFIG_DIR` and `$BRICOLO_DATA_DIR`, or the defaults under the home
    /// folder. `None` when neither is set and there is no home folder.
    pub fn from_env() -> Option<Self> {
        let env = |name| std::env::var_os(name).map(PathBuf::from);
        let home = home_dir();
        let config = env(CONFIG_ENV).or_else(|| default_config(home.as_deref()?))?;
        let data = env(DATA_ENV).or_else(|| default_data(home.as_deref()?))?;
        Some(Self { config, data })
    }

    pub fn node_toml(&self) -> PathBuf {
        self.config.join("node.toml")
    }
    pub fn rules_toml(&self) -> PathBuf {
        self.config.join("rules.toml")
    }
    /// Trusted peers and what each may call. See `bricolo-mesh`.
    pub fn peers_toml(&self) -> PathBuf {
        self.config.join("peers.toml")
    }
    pub fn permissions_toml(&self) -> PathBuf {
        self.config.join("permissions.toml")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.config.join("plugins.lock")
    }
    pub fn plugins_dir(&self) -> PathBuf {
        self.config.join("plugins")
    }
    /// Lua scripts: `scripts/*.lua` run, subfolders hold modules.
    pub fn scripts_dir(&self) -> PathBuf {
        self.config.join("scripts")
    }
    pub fn plugin_dir(&self, name: &str) -> PathBuf {
        self.plugins_dir().join(name)
    }
    /// A plugin's private folder, its only file access through WASI.
    pub fn plugin_data(&self, name: &str) -> PathBuf {
        self.data.join("data").join(name)
    }
    pub fn plugin_data_root(&self) -> PathBuf {
        self.data.join("data")
    }
    /// This node's iroh secret key, its identity on the mesh.
    pub fn node_key(&self) -> PathBuf {
        self.data.join("node.key")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.data.join("logs")
    }
    /// The control socket `bricolo status` talks to: `<data>/bricolo.sock`, or on
    /// Windows the named pipe [`pipe_name`] of the data folder.
    pub fn socket(&self) -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(pipe_name(&self.data))
        }
        #[cfg(not(windows))]
        {
            self.data.join("bricolo.sock")
        }
    }
    /// What the hub turned off, kept across restarts.
    pub fn state_toml(&self) -> PathBuf {
        self.data.join("state.toml")
    }
    /// The chords the user changed in the hub, kept across restarts.
    pub fn hotkeys_toml(&self) -> PathBuf {
        self.data.join("hotkeys.toml")
    }
}

/// The control pipe on Windows: `\\.\pipe\bricolo-<hash>`, where
/// the hash is FNV-1a over the data folder, lowercased with `\`
/// separators, since Windows paths ignore case. Two nodes with different
/// `BRICOLO_DATA_DIR` get different pipes. `bricolo-ui` computes the same name in
/// its `client.rs`; both test it against the same value.
pub fn pipe_name(data: &Path) -> String {
    let key = data.to_string_lossy().replace('/', "\\").to_lowercase();
    let key = key.trim_end_matches('\\');
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!(r"\\.\pipe\bricolo-{hash:016x}")
}

pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var_os(var).map(PathBuf::from)
}

#[cfg(not(windows))]
fn default_config(home: &Path) -> Option<PathBuf> {
    Some(home.join(".config/bricolo"))
}

#[cfg(not(windows))]
fn default_data(home: &Path) -> Option<PathBuf> {
    Some(home.join(".local/share/bricolo"))
}

#[cfg(windows)]
fn default_config(_home: &Path) -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("bricolo"))
}

#[cfg(windows)]
fn default_data(_home: &Path) -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("bricolo"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_name_follows_the_data_folder_and_ignores_case() {
        // crates/ui/src/client.rs checks the same value.
        let name = pipe_name(Path::new(r"C:\Users\Ada\AppData\Local\bricolo"));
        assert_eq!(name, r"\\.\pipe\bricolo-9857ab219b873b74");
        assert_eq!(
            pipe_name(Path::new(r"c:\users\ada\appdata\local\bricolo\")),
            name
        );
        assert_eq!(
            pipe_name(Path::new("C:/Users/Ada/AppData/Local/bricolo")),
            name
        );
        assert_ne!(pipe_name(Path::new(r"C:\Users\Ada\bricolo-test")), name);
    }
}
