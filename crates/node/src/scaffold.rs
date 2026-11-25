//! `bricolo new <name> [--template <t>]`: writes a plugin crate from
//! `templates/<t>/` and `templates/common/`.
//!
//! Inside this repository's workspace the crate uses the shared `wit/` and
//! target folder. Anywhere else it is standalone: its own `[workspace]`,
//! toolchain file and a copy of the standard library WIT.
//!
//! `bricolo new-script <name> [--template <t>]`: writes `scripts/<name>.lua`
//! from `templates/scripts/<t>.lua`.
//!
//! `bricolo new-sidecar <name> [--template <t>]`: writes a local HTTP server
//! from `templates/sidecars/<t>/` that `bricolo sidecar add` registers.
//!
//! `bricolo wit`: prints or writes the standard library WIT.
//!
//! The templates are compiled into the binary (see build.rs).

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use bricolo_runtime::manifest::valid_name;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/templates.rs"));
}

pub struct Template {
    pub name: &'static str,
    pub summary: &'static str,
}

/// `bricolo new --template`. Each is a folder under `templates/`.
pub const PLUGIN_TEMPLATES: &[Template] = &[
    Template {
        name: "plugin",
        summary: "log and kv: counts its starts. The smallest plugin",
    },
    Template {
        name: "hotkey-tool",
        summary: "hotkey, clipboard, notify: shows the clipboard text on ctrl+alt+shift+c",
    },
    Template {
        name: "voice",
        summary: "hotkey, disk, speech, tts: transcribes the newest recording on ctrl+alt+shift+t",
    },
    Template {
        name: "window-tool",
        summary: "hotkey, window: centers the focused window on ctrl+alt+shift+m",
    },
    Template {
        name: "http-service",
        summary: "timer, http, kv: checks a URL every minute, logs when it goes up or down",
    },
    Template {
        name: "dictation",
        summary: "hotkey, mic, speech, clipboard, keyboard: records on ctrl+alt+cmd+d, pastes the text",
    },
    Template {
        name: "sidecar-client",
        summary: "hotkey, clipboard, http, ui: sends the clipboard to a sidecar on ctrl+alt+shift+p, shows the answer",
    },
];

/// `bricolo new-script --template`. Each is `templates/scripts/<name>.lua`.
pub const SCRIPT_TEMPLATES: &[Template] = &[
    Template {
        name: "starter",
        summary: "a commented tour: hotkey, clipboard, tts, kv, timer",
    },
    Template {
        name: "hotkey-tool",
        summary: "hotkey, clipboard, notify: shows the clipboard text on ctrl+alt+cmd+c",
    },
    Template {
        name: "voice",
        summary: "hotkey, disk, speech, tts: transcribes the newest recording on ctrl+alt+cmd+t",
    },
    Template {
        name: "window-tool",
        summary: "hotkey, window: centers the focused window on ctrl+alt+cmd+m",
    },
    Template {
        name: "http-service",
        summary: "timer, http, kv: checks a URL every minute, logs when it goes up or down",
    },
    Template {
        name: "sidecar-client",
        summary: "hotkey, clipboard, http, ui: sends the clipboard to a sidecar on ctrl+alt+cmd+p, shows the answer",
    },
];

/// `bricolo new-sidecar --template`. Each is a folder under `templates/sidecars/`.
pub const SIDECAR_TEMPLATES: &[Template] = &[
    Template {
        name: "python",
        summary: "Python 3, standard library only (http.server), with unittest tests",
    },
    Template {
        name: "node",
        summary: "Node.js 18+, built-in http only, no npm install, with node --test tests",
    },
    Template {
        name: "rust",
        summary: "Rust, standard library only, a standalone crate with cargo tests",
    },
];

/// The port `bricolo new-sidecar` and the sidecar-client templates use when
/// `--port` is not given.
pub const DEFAULT_SIDECAR_PORT: u16 = 8800;

/// The embedded files under `templates/<dir>/`, by their path inside it.
fn template_files(dir: &str) -> impl Iterator<Item = (&'static str, &'static str)> {
    let prefix = format!("{dir}/");
    embedded::FILES
        .iter()
        .filter_map(move |(path, text)| Some((path.strip_prefix(&prefix)?, *text)))
}

fn template_file(path: &str) -> &'static str {
    embedded::FILES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, text)| *text)
        .unwrap_or_else(|| panic!("templates/{path} is not embedded"))
}

pub fn print_templates(templates: &[Template]) {
    for t in templates {
        println!("{:<14} {}", t.name, t.summary);
    }
}

fn find<'a>(templates: &'a [Template], name: &str) -> Result<&'a Template> {
    templates.iter().find(|t| t.name == name).with_context(|| {
        let names: Vec<_> = templates.iter().map(|t| t.name).collect();
        format!("no template {name:?}; pick one of {}", names.join(", "))
    })
}

/// The standard library, copied into standalone plugins.
const WIT: &[(&str, &str)] = &[
    ("types.wit", include_str!("../../../wit/types.wit")),
    ("log.wit", include_str!("../../../wit/log.wit")),
    ("timer.wit", include_str!("../../../wit/timer.wit")),
    ("hotkey.wit", include_str!("../../../wit/hotkey.wit")),
    ("tts.wit", include_str!("../../../wit/tts.wit")),
    ("speech.wit", include_str!("../../../wit/speech.wit")),
    ("disk.wit", include_str!("../../../wit/disk.wit")),
    ("clipboard.wit", include_str!("../../../wit/clipboard.wit")),
    ("kv.wit", include_str!("../../../wit/kv.wit")),
    ("window.wit", include_str!("../../../wit/window.wit")),
    ("app.wit", include_str!("../../../wit/app.wit")),
    ("notify.wit", include_str!("../../../wit/notify.wit")),
    ("shell.wit", include_str!("../../../wit/shell.wit")),
    ("http.wit", include_str!("../../../wit/http.wit")),
    ("ui.wit", include_str!("../../../wit/ui.wit")),
    ("keyboard.wit", include_str!("../../../wit/keyboard.wit")),
    ("secrets.wit", include_str!("../../../wit/secrets.wit")),
    ("mic.wit", include_str!("../../../wit/mic.wit")),
    ("audio.wit", include_str!("../../../wit/audio.wit")),
    ("system.wit", include_str!("../../../wit/system.wit")),
    ("files.wit", include_str!("../../../wit/files.wit")),
    ("mouse.wit", include_str!("../../../wit/mouse.wit")),
    ("screen.wit", include_str!("../../../wit/screen.wit")),
    ("selection.wit", include_str!("../../../wit/selection.wit")),
    ("camera.wit", include_str!("../../../wit/camera.wit")),
    (
        "claude-code.wit",
        include_str!("../../../wit/claude-code.wit"),
    ),
    (
        "accessibility.wit",
        include_str!("../../../wit/accessibility.wit"),
    ),
    ("process.wit", include_str!("../../../wit/process.wit")),
    ("device.wit", include_str!("../../../wit/device.wit")),
    ("scripting.wit", include_str!("../../../wit/scripting.wit")),
    (
        "virtual-screen.wit",
        include_str!("../../../wit/virtual-screen.wit"),
    ),
    ("world.wit", include_str!("../../../wit/world.wit")),
];

/// `port` fills `__PORT__`, which only the sidecar-client template uses.
pub fn new_plugin(name: &str, parent: &Path, template: &str, port: u16) -> Result<PathBuf> {
    let template = find(PLUGIN_TEMPLATES, template)?;
    if !valid_name(name) {
        bail!("name {name:?} must be lowercase letters, digits and dashes");
    }
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let parent = parent.canonicalize()?;
    let dir = parent.join(name);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    let crate_name = name.replace('-', "_");
    let workspace = bricolo_workspace(&parent);
    let (wit, target, standalone) = match &workspace {
        Some(root) => {
            let up = relative(&dir, root);
            (up.join("wit"), up.join("target"), "")
        }
        None => (
            PathBuf::from("wit"),
            PathBuf::from("target"),
            template_file("common/standalone.toml.tmpl"),
        ),
    };
    let wasm = target.join(format!("wasm32-wasip2/release/{crate_name}.wasm"));
    let fill = |text: &str| {
        text.replace("__NAME__", name)
            .replace("__WIT__", &slashes(&wit))
            .replace("__WASM__", &slashes(&wasm))
            .replace("__STANDALONE__", standalone)
            .replace("__PORT__", &port.to_string())
    };
    let mut files = vec![(
        "Cargo.toml".to_string(),
        fill(template_file("common/Cargo.toml.tmpl")),
    )];
    for (rel, text) in template_files(template.name) {
        files.push((rel.trim_end_matches(".tmpl").to_string(), fill(text)));
    }
    if workspace.is_none() {
        files.push((
            "rust-toolchain.toml".into(),
            template_file("common/rust-toolchain.toml").into(),
        ));
        files.push((
            ".gitignore".into(),
            template_file("common/gitignore").into(),
        ));
        for (file, text) in WIT {
            files.push((format!("wit/{file}"), (*text).to_string()));
        }
    }
    for (rel, text) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(dir)
}

/// Writes the script template `template` to `<scripts>/<name>.lua`. A
/// running node starts it at once.
/// `port` fills `__PORT__`, which only the sidecar-client template uses.
pub fn new_script(name: &str, scripts: &Path, template: &str, port: u16) -> Result<PathBuf> {
    let template = find(SCRIPT_TEMPLATES, template)?;
    let text = template_file(&format!("scripts/{}.lua", template.name));
    let name = name.strip_suffix(".lua").unwrap_or(name);
    if !bricolo_lua::valid_name(name) {
        bail!("name {name:?} must be letters, digits, dashes and underscores");
    }
    std::fs::create_dir_all(scripts)
        .with_context(|| format!("cannot create {}", scripts.display()))?;
    let path = scripts.join(format!("{name}.lua"));
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    let text = text
        .replace("__NAME__", name)
        .replace("__PORT__", &port.to_string());
    std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(path)
}

/// A sidecar `bricolo new-sidecar` wrote, and how to register it.
pub struct NewSidecar {
    pub dir: PathBuf,
    /// The arguments to `bricolo sidecar add` after the name.
    pub add_args: Vec<String>,
    /// What to run before registering it, like `cargo build --release`.
    pub build: Option<String>,
    pub test: String,
}

/// Writes the sidecar template `template` to `<parent>/<name>/`: a local
/// HTTP server on 127.0.0.1:`port` with `GET /health` and `POST /process`.
pub fn new_sidecar(name: &str, parent: &Path, template: &str, port: u16) -> Result<NewSidecar> {
    let template = find(SIDECAR_TEMPLATES, template)?;
    if !valid_name(name) {
        bail!("name {name:?} must be lowercase letters, digits and dashes");
    }
    if port == 0 {
        bail!("--port must be a port number from 1 to 65535");
    }
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let dir = parent.canonicalize()?.join(name);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    let dir_text = dir.to_string_lossy().into_owned();
    let program = |p: &str| find_program(p).unwrap_or_else(|| p.to_string());
    let (add_args, build, test) = match template.name {
        "python" => (
            vec![program("python3"), format!("{dir_text}/server.py")],
            None,
            "python3 -m unittest".to_string(),
        ),
        "node" => (
            vec![program("node"), format!("{dir_text}/server.mjs")],
            None,
            "node --test".to_string(),
        ),
        _ => (
            vec![format!("{dir_text}/target/release/{name}")],
            Some("cargo build --release".to_string()),
            "cargo test".to_string(),
        ),
    };
    let fill = |text: &str| {
        text.replace("__NAME__", name)
            .replace("__PORT__", &port.to_string())
            .replace("__DIR__", &dir_text)
            .replace("__PYTHON__", &program("python3"))
            .replace("__NODE__", &program("node"))
    };
    for (rel, text) in template_files(&format!("sidecars/{}", template.name)) {
        let rel = match rel.trim_end_matches(".tmpl") {
            "gitignore" => ".gitignore",
            other => other,
        };
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, fill(text))
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    let mut add = vec!["--command".to_string(), add_args[0].clone()];
    add.extend(["--port".to_string(), port.to_string()]);
    if add_args.len() > 1 {
        add.push("--".into());
        add.extend(add_args[1..].iter().cloned());
    }
    Ok(NewSidecar {
        dir,
        add_args: add,
        build,
        test,
    })
}

/// The absolute path of `program` on `PATH`. The node may run with
/// another `PATH` (launchd gives agents a short one), so `bricolo sidecar add`
/// gets the full path.
fn find_program(program: &str) -> Option<String> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// `bricolo wit`. With no name it lists the interfaces. `all` prints the whole
/// package, which is valid WIT as one file. A name prints that file. With
/// `out`, the files go to that folder instead.
pub fn wit(name: Option<&str>, out: Option<&Path>) -> Result<()> {
    let Some(name) = name else {
        for (file, text) in WIT {
            let doc = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("///"))
                .unwrap_or_default();
            println!("{:<10} {}", file.trim_end_matches(".wit"), doc.trim());
        }
        return Ok(());
    };
    let selected: Vec<(&str, &str)> = WIT
        .iter()
        .filter(|(file, _)| name == "all" || file.trim_end_matches(".wit") == name)
        .copied()
        .collect();
    if selected.is_empty() {
        bail!("no interface {name:?}; `bricolo wit` lists them");
    }
    match out {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
            for (file, text) in &selected {
                let path = dir.join(file);
                std::fs::write(&path, text)
                    .with_context(|| format!("cannot write {}", path.display()))?;
            }
            println!("wrote {} files to {}", selected.len(), dir.display());
        }
        None => {
            for (i, (_, text)) in selected.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                print!("{text}");
            }
        }
    }
    Ok(())
}

/// The nearest ancestor that is this repository's workspace: a Cargo
/// workspace with `wit/world.wit`.
fn bricolo_workspace(from: &Path) -> Option<PathBuf> {
    from.ancestors()
        .find(|dir| {
            let cargo = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
            cargo.contains("[workspace]") && dir.join("wit/world.wit").is_file()
        })
        .map(Path::to_path_buf)
}

/// `../` repeated from `from` up to `root`, which must be an ancestor.
fn relative(from: &Path, root: &Path) -> PathBuf {
    let depth = from
        .strip_prefix(root)
        .map(|rest| {
            rest.components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count()
        })
        .unwrap_or(0);
    (0..depth).map(|_| "..").collect()
}

/// TOML and Rust strings want forward slashes on every OS.
fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_plugin_gets_its_own_workspace_and_wit() {
        let dir = tempfile::tempdir().unwrap();
        let created =
            new_plugin("demo-plugin", dir.path(), "plugin", DEFAULT_SIDECAR_PORT).unwrap();
        let cargo = std::fs::read_to_string(created.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"demo-plugin\""));
        assert!(cargo.contains("[workspace]"));
        let manifest = std::fs::read_to_string(created.join("plugin.toml")).unwrap();
        assert!(manifest.contains("wasm = \"target/wasm32-wasip2/release/demo_plugin.wasm\""));
        bricolo_runtime::manifest::Manifest::parse(&manifest).unwrap();
        assert!(created.join("wit/world.wit").is_file());
        assert!(created.join("rust-toolchain.toml").is_file());
        let lib = std::fs::read_to_string(created.join("src/lib.rs")).unwrap();
        assert!(lib.contains("path: \"wit\""));
        assert!(new_plugin("demo-plugin", dir.path(), "plugin", DEFAULT_SIDECAR_PORT).is_err());
        assert!(new_plugin("Bad_Name", dir.path(), "plugin", DEFAULT_SIDECAR_PORT).is_err());
        assert!(new_plugin("other", dir.path(), "nope", DEFAULT_SIDECAR_PORT).is_err());
    }

    #[test]
    fn new_script_writes_a_starter_once() {
        let dir = tempfile::tempdir().unwrap();
        let scripts = dir.path().join("scripts");
        let path = new_script("speak_clip", &scripts, "starter", DEFAULT_SIDECAR_PORT).unwrap();
        assert_eq!(path, scripts.join("speak_clip.lua"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("speak_clip loaded"));
        assert!(!text.contains("__NAME__"));
        assert!(new_script("speak_clip.lua", &scripts, "starter", DEFAULT_SIDECAR_PORT).is_err());
        assert!(new_script("../escape", &scripts, "starter", DEFAULT_SIDECAR_PORT).is_err());
        assert!(new_script("other", &scripts, "nope", DEFAULT_SIDECAR_PORT).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_starter_script_runs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = bricolo_runtime::Paths::new(dir.path().join("config"), dir.path().join("data"));
        new_script(
            "starter",
            &paths.scripts_dir(),
            "starter",
            DEFAULT_SIDECAR_PORT,
        )
        .unwrap();
        let node = crate::control::Node::start(&paths, Default::default())
            .await
            .unwrap();
        let info = node.scripts.script_status("starter").await.unwrap();
        // The hotkey provider is a stub that answers `unavailable`; the
        // script ignores the result, so it keeps running.
        assert_eq!(info.state, bricolo_lua::State::Running, "{info:?}");
        let logged = node.runtime.recent_logs(Some("starter.lua"), 10);
        assert_eq!(logged[0].message, "starter loaded");
        node.shutdown().await;
    }

    #[test]
    fn plugin_inside_the_repo_uses_the_shared_wit_and_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"plugins/*\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("wit")).unwrap();
        std::fs::write(root.join("wit/world.wit"), "").unwrap();
        let created = new_plugin(
            "demo",
            &root.join("plugins"),
            "plugin",
            DEFAULT_SIDECAR_PORT,
        )
        .unwrap();
        let cargo = std::fs::read_to_string(created.join("Cargo.toml")).unwrap();
        assert!(!cargo.contains("[workspace]"));
        let manifest = std::fs::read_to_string(created.join("plugin.toml")).unwrap();
        assert!(manifest.contains("wasm = \"../../target/wasm32-wasip2/release/demo.wasm\""));
        let lib = std::fs::read_to_string(created.join("src/lib.rs")).unwrap();
        assert!(lib.contains("path: \"../../wit\""));
        assert!(!created.join("wit").exists());
    }

    #[test]
    fn every_template_is_embedded_and_fills_every_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        for t in PLUGIN_TEMPLATES {
            let name = format!("t-{}", t.name);
            let created = new_plugin(&name, dir.path(), t.name, DEFAULT_SIDECAR_PORT).unwrap();
            let manifest = std::fs::read_to_string(created.join("plugin.toml")).unwrap();
            let manifest = bricolo_runtime::manifest::Manifest::parse(&manifest).unwrap();
            assert_eq!(manifest.name, name);
            for rel in ["Cargo.toml", "src/lib.rs", "src/domain.rs"] {
                let text = std::fs::read_to_string(created.join(rel)).unwrap();
                assert!(!text.contains("__"), "{name}/{rel} has a placeholder left");
            }
            let lib = std::fs::read_to_string(created.join("src/lib.rs")).unwrap();
            assert!(lib.contains("mod domain;"), "{name}");
        }
        // Every template folder has an entry, and the other way round.
        let dirs: std::collections::BTreeSet<&str> = embedded::FILES
            .iter()
            .filter_map(|(p, _)| p.split_once('/').map(|(d, _)| d))
            .filter(|d| !["common", "scripts", "sidecars"].contains(d))
            .collect();
        let listed: std::collections::BTreeSet<&str> =
            PLUGIN_TEMPLATES.iter().map(|t| t.name).collect();
        assert_eq!(dirs, listed);
        let scripts = embedded::FILES
            .iter()
            .filter(|(p, _)| p.starts_with("scripts/"))
            .count();
        assert_eq!(scripts, SCRIPT_TEMPLATES.len());
    }

    const PLACEHOLDERS: [&str; 5] = ["__NAME__", "__PORT__", "__DIR__", "__PYTHON__", "__NODE__"];

    #[test]
    fn every_sidecar_template_is_written_and_filled() {
        let dir = tempfile::tempdir().unwrap();
        for t in SIDECAR_TEMPLATES {
            let name = format!("sc-{}", t.name);
            let created = new_sidecar(&name, dir.path(), t.name, 8123).unwrap();
            assert!(created.dir.join("README.md").is_file(), "{name}");
            let mut files = 0;
            for entry in walk(&created.dir) {
                let text = std::fs::read_to_string(&entry).unwrap();
                for p in PLACEHOLDERS {
                    assert!(!text.contains(p), "{} has {p} left", entry.display());
                }
                files += 1;
            }
            assert!(files >= 3, "{name}: {files} files");
            let readme = std::fs::read_to_string(created.dir.join("README.md")).unwrap();
            assert!(readme.contains("127.0.0.1:8123"), "{name}");
            assert!(
                readme.contains(&format!("bricolo sidecar add {name}")),
                "{name}"
            );
            assert_eq!(created.add_args[2..4], ["--port", "8123"]);
        }
        let rust = dir.path().canonicalize().unwrap().join("sc-rust");
        assert!(rust.join(".gitignore").is_file());
        let cargo = std::fs::read_to_string(rust.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"sc-rust\"") && cargo.contains("[workspace]"));
        assert!(new_sidecar("sc-rust", dir.path(), "rust", 1).is_err());
        assert!(new_sidecar("Bad", dir.path(), "rust", 1).is_err());
        assert!(new_sidecar("ok", dir.path(), "cobol", 1).is_err());
        assert!(new_sidecar("ok", dir.path(), "rust", 0).is_err());
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    /// The Python and Node.js templates, run as sidecars by the node's own
    /// supervisor, answer the contract. Skipped without the interpreter.
    #[tokio::test(flavor = "multi_thread")]
    async fn python_and_node_sidecars_answer_health_and_process() {
        use bricolo_providers::sidecar::{SidecarConfig, SidecarState, Sidecars};
        let dir = tempfile::tempdir().unwrap();
        for (template, program) in [("python", "python3"), ("node", "node")] {
            if find_program(program).is_none() {
                eprintln!("{program} missing; skipped the {template} template");
                continue;
            }
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let name = format!("sc-{template}");
            let created = new_sidecar(&name, dir.path(), template, port).unwrap();
            // add_args: --command <program> --port <n> -- <script>
            let config = SidecarConfig {
                command: created.add_args[1].clone(),
                args: created.add_args[5..].to_vec(),
                cwd: None,
                env: Default::default(),
                port,
                health_url: None,
                startup_secs: 30,
            };
            let sidecars = Sidecars::start([(name.clone(), config)].into(), None);
            let start = std::time::Instant::now();
            while sidecars.state(&name) != Some(SidecarState::Ready) {
                assert!(
                    start.elapsed() < std::time::Duration::from_secs(30),
                    "{name}: {:?}",
                    sidecars.status()
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let url = format!("http://127.0.0.1:{port}/process");
            let answer = tokio::task::spawn_blocking(move || {
                std::process::Command::new("curl")
                    .args(["-s", "--data", "hello", &url])
                    .output()
            })
            .await
            .unwrap();
            if let Ok(out) = answer {
                assert_eq!(String::from_utf8_lossy(&out.stdout), "HELLO", "{name}");
            }
            sidecars.shutdown().await;
        }
    }

    #[test]
    fn the_wit_list_matches_the_wit_folder() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wit");
        let mut on_disk: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".wit"))
            .collect();
        on_disk.sort();
        let mut listed: Vec<String> = WIT.iter().map(|(f, _)| f.to_string()).collect();
        listed.sort();
        assert_eq!(listed, on_disk, "add the new file to WIT in scaffold.rs");
        // types.wit holds the package line, so it must come first for
        // `bricolo wit all` to print one valid file.
        assert_eq!(WIT[0].0, "types.wit");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_script_template_runs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = bricolo_runtime::Paths::new(dir.path().join("config"), dir.path().join("data"));
        for t in SCRIPT_TEMPLATES {
            new_script(
                &format!("t-{}", t.name),
                &paths.scripts_dir(),
                t.name,
                DEFAULT_SIDECAR_PORT,
            )
            .unwrap();
        }
        let node = crate::control::Node::start(&paths, Default::default())
            .await
            .unwrap();
        for t in SCRIPT_TEMPLATES {
            let info = node
                .scripts
                .script_status(&format!("t-{}", t.name))
                .await
                .unwrap();
            assert_eq!(
                info.state,
                bricolo_lua::State::Running,
                "{}: {info:?}",
                t.name
            );
        }
        node.shutdown().await;
    }
}
