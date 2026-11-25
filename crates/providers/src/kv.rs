//! `kv`: a JSON file per plugin at `data/<plugin>/kv.json`, rewritten
//! atomically on each change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::CallError;

pub const MAX_KEY: usize = 256;
pub const MAX_VALUE: usize = 1 << 20;

pub trait KvProvider: Send + Sync {
    fn get(&self, plugin: &str, key: &str) -> Option<String>;
    fn set(&self, plugin: &str, key: &str, value: &str) -> Result<(), CallError>;
    fn delete(&self, plugin: &str, key: &str) -> Result<(), CallError>;
    fn keys(&self, plugin: &str) -> Vec<String>;
}

pub struct FileKv {
    data_dir: PathBuf,
    /// Serializes read-modify-write cycles.
    lock: Mutex<()>,
}

type Map = BTreeMap<String, String>;

impl FileKv {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            lock: Mutex::new(()),
        }
    }

    fn file(&self, plugin: &str) -> PathBuf {
        self.data_dir.join(plugin).join("kv.json")
    }

    fn load(&self, plugin: &str) -> Map {
        let Ok(text) = std::fs::read_to_string(self.file(plugin)) else {
            return Map::new();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(plugin, "kv.json is not valid, starting empty: {e}");
            Map::new()
        })
    }

    fn save(&self, plugin: &str, map: &Map) -> Result<(), CallError> {
        let file = self.file(plugin);
        let text = serde_json::to_string_pretty(map).map_err(CallError::failed)?;
        write_atomic(&file, text.as_bytes()).map_err(CallError::failed)
    }
}

/// Writes `bytes` to a sibling temp file, then renames it over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl KvProvider for FileKv {
    fn get(&self, plugin: &str, key: &str) -> Option<String> {
        let _guard = self.lock.lock().unwrap();
        self.load(plugin).remove(key)
    }

    fn set(&self, plugin: &str, key: &str, value: &str) -> Result<(), CallError> {
        if key.is_empty() || key.len() > MAX_KEY {
            return Err(CallError::Failed(format!(
                "key must be 1 to {MAX_KEY} bytes"
            )));
        }
        if value.len() > MAX_VALUE {
            return Err(CallError::Failed(format!(
                "value must be at most {MAX_VALUE} bytes"
            )));
        }
        let _guard = self.lock.lock().unwrap();
        let mut map = self.load(plugin);
        map.insert(key.to_string(), value.to_string());
        self.save(plugin, &map)
    }

    fn delete(&self, plugin: &str, key: &str) -> Result<(), CallError> {
        let _guard = self.lock.lock().unwrap();
        let mut map = self.load(plugin);
        if map.remove(key).is_some() {
            self.save(plugin, &map)?;
        }
        Ok(())
    }

    fn keys(&self, plugin: &str) -> Vec<String> {
        let _guard = self.lock.lock().unwrap();
        self.load(plugin).into_keys().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_persist_per_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let kv = FileKv::new(dir.path().to_path_buf());
        kv.set("a", "k", "1").unwrap();
        kv.set("b", "k", "2").unwrap();
        let again = FileKv::new(dir.path().to_path_buf());
        assert_eq!(again.get("a", "k").as_deref(), Some("1"));
        assert_eq!(again.get("b", "k").as_deref(), Some("2"));
        assert!(dir.path().join("a/kv.json").is_file());
        again.delete("a", "k").unwrap();
        assert_eq!(again.get("a", "k"), None);
        assert_eq!(again.keys("b"), ["k"]);
    }

    #[test]
    fn rejects_empty_and_oversized_keys() {
        let dir = tempfile::tempdir().unwrap();
        let kv = FileKv::new(dir.path().to_path_buf());
        assert!(kv.set("a", "", "x").is_err());
        assert!(kv.set("a", &"k".repeat(MAX_KEY + 1), "x").is_err());
    }
}
