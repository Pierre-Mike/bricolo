//! `disk`: file access limited to the paths granted at install.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

/// One `disk:` grant from `permissions.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskGrant {
    pub root: PathBuf,
    pub write: bool,
}

impl DiskGrant {
    /// Parses `disk:<path>` (read-write) or `disk:read:<path>` (read-only).
    /// `~/` expands to `home`. Returns `None` for anything else.
    pub fn parse(spec: &str, home: &Path) -> Option<Self> {
        let rest = spec.strip_prefix("disk:")?;
        let (write, path) = match rest.strip_prefix("read:") {
            Some(path) => (false, path),
            None => (true, rest),
        };
        let root = expand(path, home)?;
        Some(Self {
            root: normalize(&root)?,
            write,
        })
    }
}

/// Expands a leading `~/`; the result must be absolute.
pub fn expand(path: &str, home: &Path) -> Option<PathBuf> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if path == "~" => home.to_path_buf(),
        None => PathBuf::from(path),
    };
    path.is_absolute().then_some(path)
}

/// Resolves `.` and `..` without touching the disk. `None` if `..`
/// climbs above the root.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() || out.as_os_str().is_empty() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out)
}

/// Follows symlinks for the part of `path` that exists, so a link inside a
/// grant cannot point outside it.
fn real(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => real(parent).join(name),
        _ => path.to_path_buf(),
    }
}

/// The real path of `path` if some grant covers it, with write access when
/// `write` is set.
pub fn authorize(
    path: &str,
    grants: &[DiskGrant],
    home: &Path,
    write: bool,
) -> Result<PathBuf, CallError> {
    let full = expand(path, home)
        .and_then(|p| normalize(&p))
        .ok_or(CallError::Denied)?;
    let full = real(&full);
    let allowed = grants
        .iter()
        .any(|g| (g.write || !write) && full.starts_with(real(&g.root)));
    if allowed {
        Ok(full)
    } else {
        Err(CallError::Denied)
    }
}

#[async_trait]
pub trait DiskProvider: Send + Sync {
    async fn read(&self, caller: &Caller, path: String) -> Result<Vec<u8>, CallError>;
    async fn write(&self, caller: &Caller, path: String, data: Vec<u8>) -> Result<(), CallError>;
    async fn list(&self, caller: &Caller, path: String) -> Result<Vec<String>, CallError>;
}

/// The local file system, through Tokio's blocking pool.
pub struct LocalDisk;

/// The home folder `~/` expands to: `$HOME`, or `%USERPROFILE%` on
/// Windows.
pub fn home() -> PathBuf {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(not(windows))]
    let var = "HOME";
    std::env::var_os(var).map(PathBuf::from).unwrap_or_default()
}

fn io_error(e: std::io::Error) -> CallError {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => CallError::Denied,
        std::io::ErrorKind::NotFound => CallError::Failed("not found".into()),
        _ => CallError::failed(e),
    }
}

#[async_trait]
impl DiskProvider for LocalDisk {
    async fn read(&self, caller: &Caller, path: String) -> Result<Vec<u8>, CallError> {
        let path = authorize(&path, &caller.disk, &home(), false)?;
        tokio::fs::read(path).await.map_err(io_error)
    }

    async fn write(&self, caller: &Caller, path: String, data: Vec<u8>) -> Result<(), CallError> {
        let path = authorize(&path, &caller.disk, &home(), true)?;
        tokio::fs::write(path, data).await.map_err(io_error)
    }

    async fn list(&self, caller: &Caller, path: String) -> Result<Vec<String>, CallError> {
        let path = authorize(&path, &caller.disk, &home(), false)?;
        let mut dir = tokio::fs::read_dir(path).await.map_err(io_error)?;
        let mut names = Vec::new();
        while let Some(entry) = dir.next_entry().await.map_err(io_error)? {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_read_write_and_read_only_grants() {
        // Windows paths need a drive to be absolute.
        let (base, home, tmp) = if cfg!(windows) {
            ("C:/Users/me", "C:/Users/me", "C:/tmp/x")
        } else {
            ("/Users/me", "/Users/me", "/tmp/x")
        };
        let home = Path::new(home);
        assert_eq!(
            DiskGrant::parse("disk:~/Notes", home),
            Some(DiskGrant {
                root: Path::new(base).join("Notes"),
                write: true
            })
        );
        assert_eq!(
            DiskGrant::parse(&format!("disk:read:{tmp}"), home),
            Some(DiskGrant {
                root: tmp.into(),
                write: false
            })
        );
        assert_eq!(DiskGrant::parse("disk:relative", home), None);
        assert_eq!(DiskGrant::parse("kv", home), None);
    }

    #[test]
    fn authorize_keeps_calls_inside_grants() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("notes")).unwrap();
        std::fs::create_dir_all(home.join("docs")).unwrap();
        let grants = [
            DiskGrant::parse("disk:~/notes", home).unwrap(),
            DiskGrant::parse("disk:read:~/docs", home).unwrap(),
        ];
        assert!(authorize("~/notes/a.md", &grants, home, true).is_ok());
        assert!(authorize("~/docs/a.md", &grants, home, false).is_ok());
        assert_eq!(
            authorize("~/docs/a.md", &grants, home, true),
            Err(CallError::Denied)
        );
        assert_eq!(
            authorize("~/notes/../secret", &grants, home, false),
            Err(CallError::Denied)
        );
        assert_eq!(
            authorize("/etc/hosts", &grants, home, false),
            Err(CallError::Denied)
        );
        assert_eq!(
            authorize("notes/a.md", &grants, home, false),
            Err(CallError::Denied)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_a_grant_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("notes")).unwrap();
        std::fs::write(home.join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(home.join("secret"), home.join("notes/link")).unwrap();
        let grants = [DiskGrant::parse("disk:~/notes", home).unwrap()];
        assert_eq!(
            authorize("~/notes/link", &grants, home, false),
            Err(CallError::Denied)
        );
    }

    #[tokio::test]
    async fn local_disk_reads_writes_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let caller = Caller {
            disk: vec![DiskGrant {
                root: root.clone(),
                write: true,
            }],
            ..Caller::new("p")
        };
        let file = root.join("a.txt").to_string_lossy().into_owned();
        LocalDisk
            .write(&caller, file.clone(), b"hi".to_vec())
            .await
            .unwrap();
        assert_eq!(LocalDisk.read(&caller, file).await.unwrap(), b"hi");
        let names = LocalDisk
            .list(&caller, root.to_string_lossy().into_owned())
            .await
            .unwrap();
        assert_eq!(names, ["a.txt"]);
        assert_eq!(
            LocalDisk.read(&caller, "/etc/hosts".into()).await,
            Err(CallError::Denied)
        );
    }
}
