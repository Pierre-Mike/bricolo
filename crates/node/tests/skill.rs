//! The create-bricolo-app skill in `skills/create-bricolo-app/`: its capability
//! cheat-sheet is generated from `wit/*.wit` and the `bricolo` table in
//! docs/scripts.md, and must match them. `just skill-refs` rewrites it.
//! Its Lua examples must compile, it must follow the Agent Skills format
//! (agentskills.io), and it must not name one agent's tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const WRITE_ENV: &str = "BRICOLO_WRITE_SKILL_REFS";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// What plugin.toml or the node needs beyond the import itself.
fn notes(capability: &str) -> &'static str {
    match capability {
        "disk" => {
            "plugin.toml also needs a grant per path: `permissions = [\"disk:~/Notes\", \"disk:read:~/Documents\"]`."
        }
        "shell" => {
            "plugin.toml also needs `[shell] allow = [\"/usr/bin/say\"]` (exact absolute paths) and, for variables the plugin sets, `env = [\"NAME\"]`."
        }
        "http" => {
            "plugin.toml also needs `[http] allow = [\"api.example.com:443\"]` (lowercase host:port). Scripts may reach any host."
        }
        "speech" => {
            "Needs a `[speech]` server in node.toml; without one calls answer `unavailable`. `transcribe-file` paths must be under a `disk:` grant."
        }
        "tts" => {
            "Speaks with `say` on macOS, espeak-ng on Linux and SAPI on Windows unless node.toml names a TTS server."
        }
        "hotkey" => {
            "macOS, Linux on X11, and Windows (`cmd` is the Windows key). Elsewhere `unavailable`. A chord another program holds answers `failed`."
        }
        "window" => {
            "macOS (needs the Accessibility permission, else `denied`), Linux on X11, and Windows (physical pixels). Elsewhere `unavailable`."
        }
        "clipboard" => {
            "macOS, Linux (X11 or Wayland) and Windows. Elsewhere `unavailable`. Images come back as a PNG path in the plugin's data folder; `set-image` and `set-files` take paths in the data folder or under a `disk:` grant."
        }
        "notify" => "macOS, Linux (X11 or Wayland) and Windows. Elsewhere `unavailable`.",
        "accessibility" => {
            "macOS (needs the Accessibility permission; screenshots need Screen Recording), Windows (UI Automation) and Linux (AT-SPI). Prefer shell, disk and http; take a `snapshot`, act on its indexes, read each result's `verification`, and never submit, send, buy or delete unless asked. See docs/computer-use.md. Never routed; a peer or the MCP gateway calls it by node."
        }
        "process" => {
            "Every system, through sysinfo. `stop` and `kill` refuse the node and its parent; agents ask before using them. Never routed."
        }
        "device" => {
            "macOS, Linux (wpctl or pactl, brightnessctl, nmcli, gsettings, loginctl) and Windows (no Focus). A setting the system can't change answers `unavailable`. `lock`, `sleep` and `set-wifi` can cut the node off. Never routed."
        }
        "scripting" => {
            "AppleScript, JXA and Shortcuts on macOS (each driven app asks for Automation once); PowerShell on Windows or wherever `pwsh` is installed. As trusted as `shell`. Scripts stop after a minute. Never routed."
        }
        "virtual-screen" => {
            "Linux (Xvfb: launch, capture, click, type) and Windows (a Remote Desktop session on Windows Server with the account stored as the secrets `virtual-screen-rdp-user` and `virtual-screen-rdp-password`, else a hidden desktop with keys without modifiers); macOS answers `unavailable`. `launch` needs the program in the plugin's `[shell] allow`. In Lua it is `bricolo.virtual_screen`. Never routed."
        }
        "app" => {
            "macOS, Linux (desktop files; `running`, `frontmost`, `quit` and `set-hidden` need X11), and Windows (`.exe` names). `watch` works on macOS and Windows."
        }
        "screen" => {
            "macOS (needs the Screen Recording permission, else `denied`), Linux on X11, Wayland with `grim`, and Windows. Elsewhere `unavailable`. Never runs on a peer."
        }
        "system" => {
            "macOS (needs `bricolo run`'s main-thread loop), Linux (power from /sys; idle time from X11 or logind; keep-awake and events through logind) and Windows. Elsewhere `unavailable`. Never runs on a peer."
        }
        "ui" => {
            "Drawn by `bricolo-ui`, the hub, on macOS and Linux. The node keeps the tile, the widget and the panels while no hub runs. Switches, buttons, choices and reviews arrive as the `ui` event."
        }
        "keyboard" => {
            "macOS (needs the Accessibility permission, else `denied`), Linux on X11 (XTEST), and Windows (`denied` when the focused app runs as administrator). Elsewhere `unavailable`. Never runs on a peer."
        }
        "mouse" => {
            "macOS (needs the Accessibility permission, else `denied`), Linux on X11 (XTEST), and Windows. Elsewhere `unavailable`. Never runs on a peer."
        }
        "mic" => {
            "Needs ffmpeg (`[mic] ffmpeg` and `device` in node.toml; DirectShow on Windows). On macOS the app running `bricolo run` needs the Microphone permission. Files go to the plugin's data folder, which `speech.transcribe-file` accepts without a `disk:` grant. Never runs on a peer."
        }
        "audio" => {
            "AVAudioPlayer on macOS, ffmpeg on Linux, `unavailable` on Windows. Plays files in the plugin's data folder, or under a `disk:` grant. The node stops a plugin's sounds when it reloads; keep \"playing\" in `kv` and play again in `init`. Never runs on a peer."
        }
        "secrets" => {
            "The macOS Keychain (needs `bricolo run` in a GUI session), on Linux the Secret Service (GNOME Keyring, KWallet, KeePassXC), service `bricolo`, or the Windows Credential Manager; each plugin sees only its own secrets. Without them `unavailable`. Names are 1 to 128 letters, digits, `.`, `_` or `-`. Never runs on a peer."
        }
        "claude-code" => {
            "Needs `[claude-code] dirs` in node.toml (the folders sessions may work in) and the `claude` CLI on the node; without them calls answer `unavailable`. In Lua it is `bricolo.claude_code`. Can run on a peer through rules.toml."
        }
        _ => "",
    }
}

/// What a user asks for, and the capability (or part) that does it. The
/// first table of the cheat-sheet, for step 2 of the skill's workflow.
const WANTS: &[(&str, &str)] = &[
    ("Run something on a key chord", "`hotkey`"),
    ("Run something every N seconds, or once later", "`timer`"),
    (
        "Read or write the clipboard text",
        "`clipboard` (no change event: poll with `timer`)",
    ),
    (
        "Find, focus, move or resize windows; screen sizes",
        "`window`",
    ),
    ("Launch apps, list running ones, the app in front", "`app`"),
    ("Show a system notification", "`notify`"),
    (
        "Show a toast, a caption, a pick list, an edit box, controls (switches and buttons), a widget with values worth a glance",
        "`ui` (drawn by the hub, `bricolo-ui`)",
    ),
    (
        "Press keys or type text into the app in front",
        "`keyboard`",
    ),
    (
        "Move the pointer, click, scroll, read where the pointer is",
        "`mouse`",
    ),
    ("Record the microphone to a WAV file", "`mic`"),
    (
        "Play a sound file, once or looped, and change its volume",
        "`audio`",
    ),
    (
        "Speech to text",
        "`speech` (needs a `[speech]` server, often a sidecar)",
    ),
    (
        "Text to speech",
        "`tts` (`say` or espeak-ng, or a `[tts]` sidecar)",
    ),
    (
        "Read and write files",
        "`disk` (plugins need a `disk:` grant per folder)",
    ),
    ("Remember small values across restarts", "`kv`"),
    (
        "Call a web API or a local server, a sidecar included",
        "`http` (plugins need `[http] allow`)",
    ),
    (
        "Run a command-line program",
        "`shell` (plugins need `[shell] allow` with its absolute path)",
    ),
    ("Write to the log", "`log`"),
    (
        "OCR, an LLM, embeddings, image or audio processing, a database, a Python or Node.js package, the GPU",
        "No capability: a sidecar, called through `http`",
    ),
    (
        "Take a screenshot",
        "No capability: `shell` with `screencapture` (macOS) or `maim` (Linux), or a sidecar",
    ),
    (
        "Read the selected text in another app",
        "No capability: `keyboard` cmd+c, then `clipboard`",
    ),
    (
        "Read screen pixels, read another app's UI elements",
        "Missing. Tell the user",
    ),
];

/// The first `///` comment of a WIT file, its lines joined.
fn first_doc(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .skip_while(|l| !l.starts_with("///"))
        .map_while(|l| l.strip_prefix("///"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}

fn render() -> String {
    let wit_dir = root().join("wit");
    let mut files: Vec<(String, String)> = std::fs::read_dir(&wit_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wit"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    files.sort_by_key(|(name, _)| match name.as_str() {
        "types" => (0, name.clone()),
        "world" => (2, name.clone()),
        _ => (1, name.clone()),
    });

    // The `bricolo` library table in docs/scripts.md, grouped by capability.
    let scripts_md = std::fs::read_to_string(root().join("docs/scripts.md")).unwrap();
    let mut lua: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for line in scripts_md.lines() {
        if let Some(rest) = line.strip_prefix("| `bricolo.") {
            let cap: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            lua.entry(cap).or_default().push(line);
        }
    }

    let mut out = String::from(
        "# Capability cheat-sheet\n\n\
         Generated from `wit/*.wit` and the `bricolo` table in docs/scripts.md by `just skill-refs`. \
         Do not edit it by hand. `bricolo wit <name>` prints the same WIT from the installed `bricolo`.\n\n\
         - **Plugins** import each capability they call: list it in `imports` in plugin.toml, and \
         `bricolo check` refuses a component that imports one not listed. In Rust an interface is \
         `bricolo::stdlib::<name>` (dashes become underscores). An `async func` is awaited. Async \
         functions take owned `String`s; sync ones take `&str`.\n\
         - **Scripts** call `bricolo.<name>.<call>`. Calls that wait return their value, or `nil, err` \
         on failure. Scripts get every capability without a prompt.\n\
         - Every failure is a `call-error`: `unavailable`, `timeout`, `denied` or `failed(msg)`. \
         In Lua, `err` is `\"unavailable\"`, `\"timeout\"`, `\"denied\"` or `\"failed: <msg>\"`.\n",
    );
    out.push_str("\n## What you want, and what does it\n\n| You want to | Use |\n|---|---|\n");
    for (want, using) in WANTS {
        out.push_str(&format!("| {want} | {using} |\n"));
    }
    out.push_str("\n## Interfaces\n\n| Interface | What it does |\n|---|---|\n");
    for (name, text) in &files {
        if name == "types" || name == "world" {
            continue;
        }
        let summary = first_doc(text);
        out.push_str(&format!("| [`{name}`](#{name}) | {summary} |\n"));
    }
    for (name, text) in &files {
        let summary = first_doc(text);
        let title = match name.as_str() {
            "types" => "Shared types".to_string(),
            "world" => "The plugin world".to_string(),
            other => other.to_string(),
        };
        out.push_str(&format!("\n## {title}\n\n{summary}\n"));
        let note = notes(name);
        if !note.is_empty() {
            out.push_str(&format!("\n{note}\n"));
        }
        out.push_str(&format!("\n```wit\n{}\n```\n", text.trim_end()));
        if let Some(rows) = lua.remove(&name.replace('-', "_")) {
            out.push_str("\nIn a script:\n\n| Call | Returns | Notes |\n|---|---|---|\n");
            for row in rows {
                out.push_str(row);
                out.push('\n');
            }
        }
    }
    if !lua.is_empty() {
        out.push_str("\n## Script-only\n\n| Call | Returns | Notes |\n|---|---|---|\n");
        for row in lua.values().flatten() {
            out.push_str(row);
            out.push('\n');
        }
    }
    out
}

#[test]
fn capability_cheat_sheet_is_current() {
    let path = root().join("skills/create-bricolo-app/references/capabilities.md");
    let fresh = render();
    if std::env::var_os(WRITE_ENV).is_some() {
        std::fs::write(&path, &fresh).unwrap();
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        current == fresh,
        "{} is out of date with wit/ or docs/scripts.md; run `just skill-refs`",
        path.display()
    );
}

fn skill_dir() -> PathBuf {
    root().join("skills/create-bricolo-app")
}

/// The YAML front matter of SKILL.md as (key, value) lines; a folded
/// value (`key: >`) is joined into one line.
fn front_matter(text: &str) -> BTreeMap<String, String> {
    let rest = text
        .strip_prefix("---\n")
        .expect("SKILL.md starts with ---");
    let (front, _) = rest
        .split_once("\n---\n")
        .expect("front matter ends with ---");
    let mut out = BTreeMap::new();
    let mut key = String::new();
    for line in front.lines() {
        if let Some(cont) = line.strip_prefix("  ") {
            let value: &mut String = out.entry(key.clone()).or_default();
            if !value.is_empty() {
                value.push(' ');
            }
            value.push_str(cont.trim());
        } else if let Some((k, v)) = line.split_once(':') {
            key = k.trim().to_string();
            let v = v.trim();
            out.insert(
                key.clone(),
                if v == ">" {
                    String::new()
                } else {
                    v.to_string()
                },
            );
        }
    }
    out
}

#[test]
fn skill_follows_the_agent_skills_format() {
    let text = std::fs::read_to_string(skill_dir().join("SKILL.md")).unwrap();
    let front = front_matter(&text);
    let name = &front["name"];
    assert_eq!(name, "create-bricolo-app", "name must match the folder");
    assert!(name.len() <= 64);
    assert!(
        name.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    );
    assert!(!name.starts_with('-') && !name.ends_with('-') && !name.contains("--"));
    let description = &front["description"];
    assert!(
        !description.is_empty() && description.chars().count() <= 1024,
        "description is {} characters",
        description.chars().count()
    );
    assert!(
        front
            .get("compatibility")
            .is_none_or(|c| c.chars().count() <= 500)
    );
    let lines = text.lines().count();
    assert!(
        lines <= 250,
        "SKILL.md has {lines} lines; move details to references/"
    );
}

/// Every relative link in the skill's Markdown points at a file that
/// exists, so an agent never follows a dead reference.
#[test]
fn skill_links_resolve() {
    let mut checked = 0;
    for dir in [skill_dir(), skill_dir().join("references")] {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for target in text.split("](").skip(1).filter_map(|r| r.split_once(')')) {
                let target = target.0.split('#').next().unwrap();
                if target.is_empty() || target.contains("://") {
                    continue;
                }
                assert!(
                    dir.join(target).exists(),
                    "{} links to {target}, which does not exist",
                    path.display()
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 10);
}

/// Instructions name actions, not one agent's tools, so any agent can
/// follow them. references/agents.md may name the agents themselves.
#[test]
fn skill_is_agent_neutral() {
    let banned = [
        "Bash tool",
        "Read tool",
        "Write tool",
        "Edit tool",
        "Grep tool",
        "Glob tool",
        "the Bash",
        "the Read",
        "TodoWrite",
        "Task tool",
        "Skill tool",
        "subagent",
        "allowed-tools",
        "run_shell_command",
        "read_file",
        "apply_patch",
    ];
    let mut files = vec![skill_dir().join("SKILL.md")];
    for entry in std::fs::read_dir(skill_dir().join("references")).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|n| n != "agents.md") {
            files.push(path);
        }
    }
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        for word in banned {
            assert!(!text.contains(word), "{} names {word:?}", path.display());
        }
        assert!(
            !text.contains('\u{2014}'),
            "{} has an em dash",
            path.display()
        );
    }
}

#[test]
fn skill_examples_compile() {
    let dir = root().join("skills/create-bricolo-app/examples");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "lua") {
            let source = std::fs::read(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();
            bricolo_lua::check_syntax(&source, &name).unwrap_or_else(|e| panic!("{e}"));
            checked += 1;
        }
    }
    assert!(checked > 0);
}

/// `bricolo skill install`, `path` and `snippet` as a user runs them, against
/// a temporary HOME.
#[test]
fn bricolo_skill_install_writes_into_the_home_folder() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let bricolo = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_bricolo"))
            .args(args)
            .env("HOME", &home)
            // Windows finds the home folder through USERPROFILE.
            .env("USERPROFILE", &home)
            .env("BRICOLO_CONFIG_DIR", home.join("config"))
            .env("BRICOLO_DATA_DIR", home.join("data"))
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .current_dir(&home)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        (out.status.success(), text)
    };
    let agents = home
        .join(".agents")
        .join("skills")
        .join("create-bricolo-app");
    let claude = home
        .join(".claude")
        .join("skills")
        .join("create-bricolo-app");

    let (ok, text) = bricolo(&["skill", "path"]);
    assert!(!ok && text.contains("not installed"), "{text}");
    let (ok, text) = bricolo(&["skill", "install", "--dry-run"]);
    assert!(ok, "{text}");
    assert!(
        text.contains(&format!("would write {}", agents.display())),
        "{text}"
    );
    assert!(
        text.contains(&format!("would write {}", claude.display())),
        "{text}"
    );
    assert!(!agents.exists());

    let (ok, text) = bricolo(&[
        "skill",
        "install",
        "--agent",
        "all,gemini",
        "--dir",
        "proj/.agents/skills",
    ]);
    assert!(ok, "{text}");
    for dir in [
        &agents,
        &claude,
        &home.join(".gemini/skills/create-bricolo-app"),
        &home.join("proj/.agents/skills/create-bricolo-app"),
    ] {
        let skill = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert_eq!(
            skill,
            std::fs::read_to_string(skill_dir().join("SKILL.md")).unwrap()
        );
        assert!(dir.join("references/sidecars.md").is_file());
        assert!(!dir.join("evals").exists());
    }
    let (ok, text) = bricolo(&["skill", "path"]);
    assert!(ok, "{text}");
    assert!(text.lines().count() >= 3, "{text}");
    let (ok, text) = bricolo(&["skill", "snippet"]);
    assert!(
        ok && text.contains(&format!("{}/SKILL.md", agents.display())),
        "{text}"
    );

    // A folder that is not this skill stays.
    let cursor = home.join(".cursor/skills/create-bricolo-app");
    std::fs::create_dir_all(&cursor).unwrap();
    std::fs::write(cursor.join("notes.txt"), "mine").unwrap();
    let (ok, text) = bricolo(&["skill", "install", "--agent", "cursor"]);
    assert!(!ok && text.contains("move it away"), "{text}");
    assert!(cursor.join("notes.txt").is_file());
}
