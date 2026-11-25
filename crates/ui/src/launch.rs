//! Starting the node from the UI: finds the `bricolo` binary next to `bricolo-ui`,
//! in the app bundle's helper, or on `PATH`, and runs `bricolo run` detached, so
//! closing the UI leaves the node running.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
const BRICOLO: &str = "bricolo.exe";
#[cfg(not(windows))]
const BRICOLO: &str = "bricolo";

/// `bricolo` next to this executable, else in the bundle's helper app, else
/// the first one on `PATH`.
pub fn find_bricolo() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| find_near(exe.parent()?))
        .or_else(|| find_in(std::env::var_os("PATH")?.as_os_str()))
}

/// `bricolo` in `dir`, or, when `dir` is an app's Contents/MacOS, in the helper
/// app Contents/Helpers/bricolo.app that holds the node (docs/install.md).
pub fn find_near(dir: &Path) -> Option<PathBuf> {
    [
        dir.join(BRICOLO),
        dir.join("../Helpers/bricolo.app/Contents/MacOS")
            .join(BRICOLO),
    ]
    .into_iter()
    .find(|p| is_program(p))
}

/// The first `bricolo` in a `PATH`-style list of folders.
pub fn find_in(path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(BRICOLO))
        .find(|p| is_program(p))
}

fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Runs `<bricolo> run` in its own process group (on Windows with a hidden
/// console), with the UI's environment
/// (so `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR` carry over). Its output goes to
/// `log`; the node also writes its daily log file. A thread waits for the
/// child so it never lingers as a zombie.
pub fn start_node(bricolo: &Path, log: Option<&Path>) -> std::io::Result<u32> {
    let mut cmd = Command::new(bricolo);
    cmd.arg("run").stdin(Stdio::null());
    match log {
        Some(path) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            cmd.stdout(file.try_clone()?).stderr(file);
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // Its own console process group, so Ctrl+C in a terminal that started
    // bricolo-ui does not reach it, and a hidden console: bricolo is a console
    // program, and without it Windows opens a console window for the node.
    // The sidecars share that console, so the node can send them
    // Ctrl+Break.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || child.wait());
    Ok(pid)
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn finds_an_executable_bricolo_on_the_path() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Not executable: skipped.
        std::fs::write(a.path().join("bricolo"), "").unwrap();
        let bricolo = b.path().join("bricolo");
        std::fs::write(&bricolo, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bricolo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([a.path(), b.path()]).unwrap();
        assert_eq!(find_in(&path), Some(bricolo));
        assert_eq!(find_in(&std::env::join_paths([a.path()]).unwrap()), None);
    }

    #[test]
    fn finds_bricolo_in_the_bundles_helper_app() {
        let app = tempfile::tempdir().unwrap();
        let macos = app.path().join("Contents/MacOS");
        let helper = app
            .path()
            .join("Contents/Helpers/bricolo.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        assert_eq!(find_near(&macos), None);
        std::fs::create_dir_all(&helper).unwrap();
        std::fs::write(helper.join("bricolo"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            helper.join("bricolo"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let found = find_near(&macos).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            helper.join("bricolo").canonicalize().unwrap()
        );
    }

    #[test]
    fn start_node_runs_bricolo_run_with_output_in_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let bricolo = dir.path().join("bricolo");
        std::fs::write(&bricolo, "#!/bin/sh\necho \"bricolo $1\"\n").unwrap();
        std::fs::set_permissions(&bricolo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = dir.path().join("logs/bricolo-run.log");
        start_node(&bricolo, Some(&log)).unwrap();
        let start = std::time::Instant::now();
        while std::fs::read_to_string(&log).unwrap_or_default() != "bricolo run\n" {
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
