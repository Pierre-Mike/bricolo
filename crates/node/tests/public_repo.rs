//! The repository is public, so what it holds must work on someone else's
//! machine and name nobody's home folder. These tests read every tracked
//! file (`git ls-files`) and fail on:
//!
//! - a home folder other than the placeholders (`/Users/me`, `/home/me`,
//!   `C:/Users/Ada`, ...),
//! - a path into `~/.hammerspoon`, which the apps came from and no reader has,
//! - a Python sidecar that imports a package without naming it in an inline
//!   script block (`# /// script`), so that it only runs with a venv someone
//!   built by hand. Sidecars run with `uv run --script`.
//!
//! They skip themselves when `git` or `python3` is missing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every tracked file, or `None` outside a git checkout.
fn tracked() -> Option<Vec<String>> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|f| !f.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Folder names allowed after `/Users/`, `/home/` or `C:\Users\`: the
/// placeholders the docs use, and system folders.
const PLACEHOLDERS: &[&str] = &["me", "you", "Ada", "Public", "data", "config"];

const THIS_FILE: &str = "crates/node/tests/public_repo.rs";

/// The home folders named in `text` that are not placeholders.
fn home_folders(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for prefix in ["/Users/", "/home/", "Users\\", "Users/"] {
        let mut rest = text;
        while let Some(at) = rest.find(prefix) {
            let after = &rest[at + prefix.len()..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                .collect();
            // "Users/" also matches inside "/Users/"; "C:/Users/" and
            // "C:\Users\" are the only other forms that name a home.
            let before = &rest[..at];
            let is_home = match prefix {
                "Users\\" | "Users/" => before.ends_with(":\\") || before.ends_with(":/"),
                _ => true,
            };
            if is_home && !name.is_empty() && !PLACEHOLDERS.contains(&name.as_str()) {
                found.push(format!("{prefix}{name}"));
            }
            rest = &after[name.len().min(after.len())..];
        }
    }
    found
}

#[test]
fn home_folders_are_placeholders() {
    assert!(home_folders("/Users/me/x C:/Users/Ada /home/me").is_empty());
    assert_eq!(home_folders("at /Users/someone/x"), ["/Users/someone"]);
    assert_eq!(home_folders(r"C:\Users\someone\x"), [r"Users\someone"]);

    let Some(files) = tracked() else {
        eprintln!("skipped: not a git checkout");
        return;
    };
    let mut bad = Vec::new();
    for f in files.iter().filter(|f| *f != THIS_FILE) {
        let Ok(text) = std::fs::read_to_string(root().join(f)) else {
            continue; // binary
        };
        for (n, line) in text.lines().enumerate() {
            for home in home_folders(line) {
                bad.push(format!("{f}:{}: {home}", n + 1));
            }
            if line.contains(".hammerspoon/") {
                bad.push(format!("{f}:{}: a path into ~/.hammerspoon", n + 1));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "use /Users/me, /path/to/bricolo or a path in this repository instead:\n{}",
        bad.join("\n")
    );
}

/// The folder of the sidecar that holds `file`, if any.
fn sidecar_folder(file: &str) -> Option<String> {
    let parts: Vec<&str> = file.split('/').collect();
    match parts.as_slice() {
        ["apps", "shared", "sidecars", name, _, ..] => Some(format!("apps/shared/sidecars/{name}")),
        ["apps", app, "sidecar", _, ..] => Some(format!("apps/{app}/sidecar")),
        ["templates", "sidecars", lang, _, ..] => Some(format!("templates/sidecars/{lang}")),
        _ => None,
    }
}

/// Reads `{folder: [files]}` as JSON on stdin and prints each folder whose
/// Python imports a package that is neither standard nor in the folder,
/// while no file there has a script block. Tests are left out: they may
/// import pytest.
const CHECK: &str = r#"
import ast, json, os, sys
root = sys.argv[1]
std = set(sys.stdlib_module_names) | {"__future__"}
for folder, files in sorted(json.load(sys.stdin).items()):
    py = [f for f in files if f.endswith(".py")]
    local = set()
    for f in files:
        rel = os.path.relpath(f, folder).split(os.sep)
        local.update(p[:-3] if p.endswith(".py") else p for p in rel)
    third, block = set(), False
    for f in py:
        src = open(os.path.join(root, f), encoding="utf-8").read()
        block = block or "\n# /// script\n" in "\n" + src
        name = os.path.basename(f)
        if name.startswith("test_") or "/tests/" in f:
            continue
        for node in ast.walk(ast.parse(src)):
            if isinstance(node, ast.Import):
                mods = [a.name for a in node.names]
            elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                mods = [node.module]
            else:
                continue
            third.update(m.split(".")[0] for m in mods)
    third -= std | local
    if third and not block:
        print(f"{folder}: imports {', '.join(sorted(third))} with no script block")
"#;

#[test]
fn python_sidecars_name_their_packages() {
    let Some(files) = tracked() else {
        eprintln!("skipped: not a git checkout");
        return;
    };
    let mut folders: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for f in &files {
        if let Some(folder) = sidecar_folder(f) {
            folders.entry(folder).or_default().push(f.clone());
        }
    }
    assert!(folders.keys().any(|f| f == "apps/shared/sidecars/parakeet"));

    let child = Command::new("python3")
        .arg("-c")
        .arg(CHECK)
        .arg(root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("skipped: no python3");
        return;
    };
    let input = serde_json::to_vec(&folders).unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), &input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "the check failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(
        report.trim().is_empty(),
        "name these packages in an inline `# /// script` block and run the sidecar with `uv run --script`:\n{report}"
    );
}
