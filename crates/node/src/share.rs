//! Plugin sharing between nodes: serving installed plugins to peers by the
//! sha256 of their `plugin.wasm`, and fetching one for
//! `bricolo install peer:<peer>/<sha256>`.
//!
//! The mesh only moves the bundle. This side finds what `plugins.lock`
//! pins, and checks the hash of what arrives before anything installs it.

use std::path::PathBuf;

use bricolo_mesh::{Mesh, PluginStore};
use bricolo_providers::{CallError, Caller};
use bricolo_runtime::Paths;
use bricolo_runtime::bundle::{self, PLUGIN_SUFFIX};
use bricolo_runtime::install;
use bricolo_runtime::lock::{Lock, sha256_hex};
use bricolo_runtime::manifest::Manifest;

/// Serves the plugins in `plugins.lock` whose installed component still
/// has the locked hash.
pub struct LockStore(pub Paths);

impl PluginStore for LockStore {
    fn bundle(&self, sha256: &str) -> Result<Vec<u8>, CallError> {
        let lock = Lock::load(&self.0.lock_file()).map_err(CallError::failed)?;
        let Some(name) = lock
            .plugins
            .iter()
            .find(|(_, entry)| entry.sha256 == sha256)
            .map(|(name, _)| name.clone())
        else {
            return Err(CallError::Unavailable);
        };
        let dir = self.0.plugin_dir(&name);
        let manifest = Manifest::load(&dir.join("plugin.toml")).map_err(CallError::failed)?;
        let wasm_path = install::wasm_path(&dir, &manifest).map_err(CallError::failed)?;
        let wasm = std::fs::read(&wasm_path).map_err(CallError::failed)?;
        // Changed since install: not what the caller asked for.
        if sha256_hex(&wasm) != sha256 {
            return Err(CallError::Unavailable);
        }
        let files = bundle::plugin_files(&manifest, &wasm).map_err(CallError::failed)?;
        bundle::pack(&files).map_err(CallError::failed)
    }
}

/// Checks that `bytes` is a plugin bundle whose component hashes to
/// `sha256`, and returns the plugin's name.
pub fn verify(bytes: &[u8], sha256: &str) -> Result<String, String> {
    let files = bundle::unpack(bytes).map_err(|e| e.to_string())?;
    let (manifest, wasm) = bundle::plugin_parts(&files).map_err(|e| e.to_string())?;
    let got = sha256_hex(&wasm);
    if got != sha256 {
        return Err(format!(
            "the peer sent {} with sha256 {got}, not {sha256}",
            manifest.name
        ));
    }
    Ok(manifest.name)
}

/// Fetches the plugin from `peer`, checks it, and saves the bundle under
/// the data folder's `downloads/`. Returns its path.
pub async fn fetch(
    mesh: &Mesh,
    paths: &Paths,
    peer: &str,
    sha256: &str,
) -> Result<PathBuf, String> {
    let bytes = mesh
        .fetch_plugin(peer, sha256, &Caller::new("bricolo-install"))
        .await
        .map_err(|e| match e {
            CallError::Denied => format!("{peer} does not allow this node to fetch plugins"),
            CallError::Unavailable => {
                format!("{peer} is offline or has no plugin with that sha256")
            }
            other => format!("cannot fetch from {peer}: {other}"),
        })?;
    let name = verify(&bytes, sha256)?;
    let dir = paths.data.join("downloads");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}-{}{PLUGIN_SUFFIX}", &sha256[..12]));
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    tracing::info!(%peer, plugin = %name, "fetched plugin {sha256}");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_checks_the_component_hash() {
        let manifest = Manifest::parse("name = \"clip\"\nversion = \"0.1.0\"\n").unwrap();
        let files = bundle::plugin_files(&manifest, b"\0asm-fake").unwrap();
        let bytes = bundle::pack(&files).unwrap();
        let sha = sha256_hex(b"\0asm-fake");
        assert_eq!(verify(&bytes, &sha).unwrap(), "clip");
        let err = verify(&bytes, &"00".repeat(32)).unwrap_err();
        assert!(err.contains("not 0000"), "{err}");
        assert!(verify(b"junk", &sha).is_err());
    }

    #[test]
    fn the_store_serves_only_the_locked_hash() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        let plugin = paths.plugin_dir("clip");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(
            plugin.join("plugin.toml"),
            "name = \"clip\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(plugin.join("plugin.wasm"), b"\0asm-v1").unwrap();
        let sha = sha256_hex(b"\0asm-v1");
        let lock = format!(
            "[plugins.clip]\nsource = \"path:/x\"\nversion = \"0.1.0\"\nsha256 = \"{sha}\"\n"
        );
        std::fs::write(paths.lock_file(), lock).unwrap();
        let store = LockStore(paths.clone());
        let bytes = store.bundle(&sha).unwrap();
        assert_eq!(verify(&bytes, &sha).unwrap(), "clip");
        assert_eq!(store.bundle(&"11".repeat(32)), Err(CallError::Unavailable));
        // The component changed after install: no longer served.
        std::fs::write(plugin.join("plugin.wasm"), b"\0asm-v2").unwrap();
        assert_eq!(store.bundle(&sha), Err(CallError::Unavailable));
    }
}
