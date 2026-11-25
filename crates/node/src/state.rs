//! `<data>/state.toml`: the plugins and scripts turned off in the hub. The
//! node writes it; it lives next to `node.key` rather than in the config
//! folder, because it is state, not configuration.
//!
//! ```toml
//! disabled_plugins = ["window-tiler"]
//! disabled_scripts = ["speak"]
//! ```

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeState {
    #[serde(default)]
    pub disabled_plugins: BTreeSet<String>,
    #[serde(default)]
    pub disabled_scripts: BTreeSet<String>,
}

impl NodeState {
    /// A missing file is an empty state.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// Writes a temporary file and renames it, so a crash never leaves
    /// half a file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = format!(
            "# Written by bricolo: what the hub turned off.\n{}",
            toml::to_string(self)?
        );
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        assert_eq!(NodeState::load(&path).unwrap(), NodeState::default());
        let state = NodeState {
            disabled_plugins: ["tiler".to_string()].into(),
            disabled_scripts: ["speak".to_string()].into(),
        };
        state.save(&path).unwrap();
        assert_eq!(NodeState::load(&path).unwrap(), state);
        std::fs::write(&path, "colour = 1").unwrap();
        assert!(NodeState::load(&path).is_err());
    }
}
