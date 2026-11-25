//! Bundles: what `bricolo pack` writes for a GitHub release and `bricolo install`
//! reads back.
//!
//! - `<name>.rhplugin.tar.gz` holds `plugin.toml` and `plugin.wasm` at its
//!   root. The manifest has no `wasm` key.
//! - `<name>.rhscripts.tar.gz` holds Lua scripts: `.lua` files at its root
//!   run as scripts, `.lua` files in subfolders are modules.
//!
//! Both are gzip-compressed tar files holding regular files only. `pack`
//! writes the same bytes for the same files (sorted names, mtime 0, mode
//! 0644), so packing a release twice gives the same sha256.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::manifest::Manifest;

pub const PLUGIN_SUFFIX: &str = ".rhplugin.tar.gz";
pub const SCRIPTS_SUFFIX: &str = ".rhscripts.tar.gz";
/// The largest download, and the most a bundle may unpack to.
pub const MAX_SIZE: u64 = 64 << 20;

/// Files by their `/`-separated path relative to the bundle root.
pub type Files = BTreeMap<String, Vec<u8>>;

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

pub fn pack(files: &Files) -> Result<Vec<u8>> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        tar.append_data(&mut header, path, bytes.as_slice())
            .map_err(|e| invalid(format!("cannot pack {path}: {e}")))?;
    }
    let gz = tar
        .into_inner()
        .map_err(|e| invalid(format!("cannot pack: {e}")))?;
    gz.finish()
        .map_err(|e| invalid(format!("cannot pack: {e}")))
}

/// Reads a bundle. Refuses links, paths that leave the root, repeated
/// paths, and more than [`MAX_SIZE`] bytes in total.
pub fn unpack(bytes: &[u8]) -> Result<Files> {
    let bad = |e: std::io::Error| invalid(format!("not a valid bundle: {e}"));
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut files = Files::new();
    let mut total = 0u64;
    for entry in archive.entries().map_err(bad)? {
        let entry = entry.map_err(bad)?;
        let kind = entry.header().entry_type();
        let path = entry.path().map_err(bad)?.to_string_lossy().into_owned();
        let path = path.strip_prefix("./").unwrap_or(&path).to_string();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(invalid(format!(
                "bundle entry {path:?} is not a regular file"
            )));
        }
        if !safe_path(&path) {
            return Err(invalid(format!(
                "bundle entry {path:?} leaves the bundle root"
            )));
        }
        let mut data = Vec::new();
        entry
            .take(MAX_SIZE + 1 - total)
            .read_to_end(&mut data)
            .map_err(bad)?;
        total += data.len() as u64;
        if total > MAX_SIZE {
            return Err(invalid(format!(
                "bundle unpacks to more than {} MiB",
                MAX_SIZE >> 20
            )));
        }
        if files.insert(path.clone(), data).is_some() {
            return Err(invalid(format!("bundle holds {path:?} twice")));
        }
    }
    Ok(files)
}

/// A relative `/`-separated path with no `.`, `..` or empty parts.
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The sha256 of a set of files: each path, a zero byte, its length and
/// its bytes, in path order. It doesn't depend on how they were packed.
pub fn content_hash(files: &Files) -> String {
    let mut hash = Sha256::new();
    for (path, bytes) in files {
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The manifest and component of a plugin bundle. Other files, such as a
/// README, are ignored.
pub fn plugin_parts(files: &Files) -> Result<(Manifest, Vec<u8>)> {
    let (Some(toml), Some(wasm)) = (files.get("plugin.toml"), files.get("plugin.wasm")) else {
        return Err(invalid(
            "a plugin bundle needs plugin.toml and plugin.wasm at its root",
        ));
    };
    let text = std::str::from_utf8(toml).map_err(|_| invalid("plugin.toml is not UTF-8"))?;
    let mut manifest = Manifest::parse(text).map_err(|e| invalid(format!("plugin.toml: {e}")))?;
    manifest.wasm = None;
    Ok((manifest, wasm.clone()))
}

/// The files of a plugin bundle: the manifest without its `wasm` key, and
/// the component.
pub fn plugin_files(manifest: &Manifest, wasm: &[u8]) -> Result<Files> {
    let mut installed = manifest.clone();
    installed.wasm = None;
    let text = toml::to_string_pretty(&installed).map_err(|e| invalid(e.to_string()))?;
    Ok(Files::from([
        ("plugin.toml".to_string(), text.into_bytes()),
        ("plugin.wasm".to_string(), wasm.to_vec()),
    ]))
}

/// The same rule as script names in bricolo-lua: ASCII letters, digits, `-`
/// and `_`.
pub fn valid_script_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Checks a scripts bundle and returns the names of the scripts that run:
/// the `.lua` files at its root. Every file must be a `.lua` file.
pub fn script_names(files: &Files) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for path in files.keys() {
        let Some(stem) = path.strip_suffix(".lua") else {
            return Err(invalid(format!(
                "{path} is not a .lua file; a scripts bundle holds only Lua"
            )));
        };
        if !path.contains('/') {
            if !valid_script_name(stem) {
                return Err(invalid(format!(
                    "script name {stem:?} must be letters, digits, dashes and underscores"
                )));
            }
            names.push(stem.to_string());
        }
    }
    if names.is_empty() {
        return Err(invalid("the bundle has no .lua file at its root"));
    }
    Ok(names)
}

/// The Lua files in `dir` and its subfolders, as a scripts bundle would
/// hold them. Hidden files and folders are skipped.
pub fn read_scripts_dir(dir: &Path) -> Result<Files> {
    fn walk(dir: &Path, prefix: &str, files: &mut Files) -> Result<()> {
        let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(dir, e))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let rel = format!("{prefix}{name}");
            if path.is_dir() {
                walk(&path, &format!("{rel}/"), files)?;
            } else if name.ends_with(".lua") {
                let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
                files.insert(rel, bytes);
            }
        }
        Ok(())
    }
    let mut files = Files::new();
    walk(dir, "", &mut files)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Files {
        list.iter()
            .map(|(p, t)| (p.to_string(), t.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn packs_the_same_bytes_and_round_trips() {
        let set = files(&[("a.lua", "x"), ("lib/b.lua", "yy")]);
        let one = pack(&set).unwrap();
        assert_eq!(one, pack(&set).unwrap());
        assert_eq!(unpack(&one).unwrap(), set);
        assert_eq!(content_hash(&set), content_hash(&unpack(&one).unwrap()));
        assert_ne!(
            content_hash(&set),
            content_hash(&files(&[("a.lua", "x"), ("lib/b.lua", "yz")]))
        );
    }

    #[test]
    fn refuses_paths_that_leave_the_root() {
        for bad in ["../x.lua", "/etc/x", "a/../../x", "", "a//b", "./"] {
            assert!(!safe_path(bad), "{bad}");
        }
        assert!(safe_path("lib/text.lua"));
        // tar's builder refuses `..` itself, so write the header by hand.
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..9].copy_from_slice(b"../x.lua\0");
        header.set_size(1);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut tar = tar::Builder::new(Vec::new());
        tar.append(&header, &b"x"[..]).unwrap();
        let raw = tar.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &raw).unwrap();
        let err = unpack(&gz.finish().unwrap()).unwrap_err();
        assert!(err.to_string().contains("leaves the bundle root"), "{err}");
        assert!(unpack(b"not a gzip").is_err());
    }

    #[test]
    fn script_bundles_hold_lua_with_a_script_at_the_root() {
        let ok = files(&[("speak.lua", ""), ("lib/text.lua", "")]);
        assert_eq!(script_names(&ok).unwrap(), ["speak"]);
        assert!(script_names(&files(&[("lib/text.lua", "")])).is_err());
        assert!(script_names(&files(&[("a.lua", ""), ("run.sh", "")])).is_err());
        assert!(script_names(&files(&[("bad name.lua", "")])).is_err());
    }

    #[test]
    fn plugin_bundles_drop_the_wasm_path() {
        let manifest = Manifest::parse(
            "name = \"demo\"\nversion = \"1.0.0\"\nimports = [\"log\"]\nwasm = \"target/x.wasm\"",
        )
        .unwrap();
        let set = plugin_files(&manifest, b"\0asm").unwrap();
        let (back, wasm) = plugin_parts(&set).unwrap();
        assert_eq!(back.wasm, None);
        assert_eq!(back.name, "demo");
        assert_eq!(wasm, b"\0asm");
        assert!(plugin_parts(&files(&[("plugin.toml", "")])).is_err());
    }

    #[test]
    fn reads_lua_files_from_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("lib")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("a.lua"), "x").unwrap();
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        std::fs::write(dir.path().join("lib/b.lua"), "y").unwrap();
        std::fs::write(dir.path().join(".git/c.lua"), "z").unwrap();
        let read = read_scripts_dir(dir.path()).unwrap();
        assert_eq!(read, files(&[("a.lua", "x"), ("lib/b.lua", "y")]));
    }
}
