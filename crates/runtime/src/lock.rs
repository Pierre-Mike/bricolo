//! `plugins.lock`: the source, version and sha256 of each installed plugin,
//! and of each script bundle `bricolo install` copied into `scripts/`.
//!
//! ```toml
//! [plugins.hello]
//! source = "path:/Users/me/bricolo/plugins/hello"
//! version = "0.1.0"
//! sha256 = "9f2c…"
//!
//! [plugins.clip-tool]
//! source = "github:me/clip-tool@v1.2.0"
//! version = "1.2.0"
//! sha256 = "51ab…"
//!
//! [scripts.speak]
//! source = "github:me/my-scripts@v3"
//! version = "v3"
//! sha256 = "07de…"
//! files = ["speak.lua", "lib/text.lua"]
//! ```
//!
//! A plugin's `sha256` is the hash of its `plugin.wasm`. A script bundle's
//! is [`crate::bundle::content_hash`] of its files. Scripts you write by
//! hand in `scripts/` have no entry.

use std::collections::BTreeMap;
use std::path::Path;

use bricolo_providers::kv::write_atomic;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LockEntry {
    pub source: String,
    pub version: String,
    pub sha256: String,
}

/// Scripts that `bricolo install` copied, from one `.lua` file or one bundle.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptEntry {
    pub source: String,
    pub version: String,
    pub sha256: String,
    /// The files written, relative to `scripts/`.
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    #[serde(default)]
    pub plugins: BTreeMap<String, LockEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scripts: BTreeMap<String, ScriptEntry>,
}

impl Lock {
    /// A missing file is an empty lock.
    pub fn load(path: &Path) -> Result<Self> {
        load_toml(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_toml(path, self)
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn load_toml<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| Error::parse(path, e.message())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::io(path, e)),
    }
}

pub(crate) fn save_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).map_err(|e| Error::Invalid(e.to_string()))?;
    write_atomic(path, text.as_bytes()).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_missing_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins.lock");
        assert_eq!(Lock::load(&path).unwrap(), Lock::default());
        let mut lock = Lock::default();
        lock.plugins.insert(
            "hello".into(),
            LockEntry {
                source: "path:/x".into(),
                version: "0.1.0".into(),
                sha256: sha256_hex(b"abc"),
            },
        );
        lock.save(&path).unwrap();
        assert_eq!(Lock::load(&path).unwrap(), lock);
        // A lock with no scripts writes no [scripts] table, as before.
        assert!(!std::fs::read_to_string(&path).unwrap().contains("scripts"));
        lock.scripts.insert(
            "speak".into(),
            ScriptEntry {
                source: "github:me/s@v1".into(),
                version: "v1".into(),
                sha256: sha256_hex(b"x"),
                files: vec!["speak.lua".into()],
            },
        );
        lock.save(&path).unwrap();
        assert_eq!(Lock::load(&path).unwrap(), lock);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
