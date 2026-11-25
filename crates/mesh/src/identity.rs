//! `node.key`: this node's iroh secret key, created on first use. Its
//! public half, the endpoint id, is how other nodes name this one in their
//! `peers.toml`.

use std::io::Write;
use std::path::Path;

use iroh::SecretKey;

use crate::error::MeshError;

/// Reads `path`, or creates it with a new key (mode 0600 on Unix).
pub fn load_or_create(path: &Path) -> Result<SecretKey, MeshError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            warn_if_readable(path);
            text.trim().parse().map_err(|e| MeshError::Invalid {
                file: "node.key",
                message: format!("{}: {e}", path.display()),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => create(path),
        Err(source) => Err(MeshError::Read {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn create(path: &Path) -> Result<SecretKey, MeshError> {
    let write_err = |source| MeshError::Write {
        path: path.display().to_string(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(write_err)?;
    }
    let key = SecretKey::generate();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(write_err)?;
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    writeln!(file, "{hex}").map_err(write_err)?;
    tracing::info!(path = %path.display(), id = %key.public(), "created node key");
    Ok(key)
}

#[cfg(unix)]
fn warn_if_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        tracing::warn!(
            path = %path.display(),
            "node.key is readable by other users; run chmod 600 on it"
        );
    }
}

#[cfg(not(unix))]
fn warn_if_readable(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_then_reads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/node.key");
        let first = load_or_create(&path).unwrap();
        let again = load_or_create(&path).unwrap();
        assert_eq!(first.public(), again.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_damaged_key_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("node.key");
        std::fs::write(&path, "not a key").unwrap();
        assert!(load_or_create(&path).is_err());
    }
}
