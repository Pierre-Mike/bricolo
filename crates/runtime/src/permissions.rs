//! `permissions.toml`: what you approved for each plugin at install.
//!
//! ```toml
//! [hello]
//! granted = ["log", "kv", "timer"]
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::lock::{load_toml, save_toml};
use crate::manifest::Manifest;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    #[serde(default)]
    pub granted: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Permissions {
    pub plugins: BTreeMap<String, Grant>,
}

impl Permissions {
    pub fn load(path: &Path) -> Result<Self> {
        load_toml(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_toml(path, self)
    }

    pub fn granted(&self, plugin: &str) -> &[String] {
        self.plugins.get(plugin).map_or(&[], |g| &g.granted)
    }

    /// What `manifest` asks for that has not been granted.
    pub fn missing(&self, manifest: &Manifest) -> Vec<String> {
        let granted = self.granted(&manifest.name);
        manifest
            .requested()
            .into_iter()
            .filter(|r| !granted.contains(r))
            .collect()
    }

    pub fn grant(&mut self, manifest: &Manifest) {
        self.plugins.insert(
            manifest.name.clone(),
            Grant {
                granted: manifest.requested(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_lists_what_was_not_approved() {
        let m = Manifest::parse(
            "name = \"a\"\nversion = \"1\"\nimports = [\"log\", \"disk\"]\npermissions = [\"disk:~/n\"]",
        )
        .unwrap();
        let mut p = Permissions::default();
        assert_eq!(p.missing(&m), ["log", "disk", "disk:~/n"]);
        p.grant(&m);
        assert!(p.missing(&m).is_empty());
        let text = toml::to_string(&p).unwrap();
        assert!(text.contains("[a]"), "{text}");
        let back: Permissions = toml::from_str(&text).unwrap();
        assert_eq!(back, p);
    }
}
