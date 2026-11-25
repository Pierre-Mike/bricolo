//! The hub's Library view: `{"cmd":"library"}` answers with the standard
//! library, who may call each capability, and the OS grants the node
//! holds; `{"cmd":"open-settings","grant":"microphone"}` opens the System
//! Settings pane where you turn a grant on or off.

use std::collections::BTreeMap;
use std::path::Path;

use bricolo_providers::catalog::Capability;
use bricolo_providers::permissions::{self, GRANTS, Permissions};
use serde_json::{Value, json};

/// What the node knows, gathered by the caller so this stays testable.
pub struct Facts {
    pub capabilities: Vec<Capability>,
    pub held: Permissions,
    pub not_asked: Vec<&'static str>,
    pub holder: String,
    /// Installed plugin to the capabilities approved in permissions.toml,
    /// as written there (`disk:~/Notes`).
    pub plugin_grants: BTreeMap<String, Vec<String>>,
    /// Installed scripts. Scripts get every capability.
    pub scripts: Vec<String>,
}

pub fn answer(facts: &Facts) -> Value {
    let capabilities: Vec<Value> = facts
        .capabilities
        .iter()
        .map(|c| {
            let plugins: Vec<&str> = facts
                .plugin_grants
                .iter()
                .filter(|(_, granted)| granted.iter().any(|g| g.split(':').next() == Some(c.name)))
                .map(|(name, _)| name.as_str())
                .collect();
            json!({
                "name": c.name,
                "summary": c.summary,
                "needs": c.needs,
                "note": c.note,
                "also": c.also,
                "plugins": plugins,
            })
        })
        .collect();
    let grants: Vec<Value> = GRANTS
        .iter()
        .map(|g| {
            let state = match facts.held.get(g.name) {
                None => "n/a",
                Some(true) => "granted",
                Some(false) if facts.not_asked.contains(&g.name) => "not-asked",
                Some(false) => "missing",
            };
            json!({ "name": g.name, "label": g.label, "state": state })
        })
        .collect();
    json!({
        "os": std::env::consts::OS,
        "holder": facts.holder,
        "grants": grants,
        "capabilities": capabilities,
        "scripts": facts.scripts,
    })
}

/// Who macOS gives the grants to: the app bundle around the `bricolo` binary,
/// else the terminal that started it, else the binary itself.
pub fn holder(exe: &Path, term_program: Option<&str>) -> String {
    if let Some(bundle) = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
    {
        let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap_or_default();
        if let Some(name) = plist_string(&plist, "CFBundleName") {
            return name;
        }
        if let Some(stem) = bundle.file_stem() {
            return stem.to_string_lossy().into_owned();
        }
    }
    match term_program {
        Some(t) if !t.is_empty() => format!("your terminal ({t})"),
        _ => format!("the bricolo program ({})", exe.display()),
    }
}

/// The `<string>` after `<key>key</key>` in an XML property list.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string()).filter(|s| !s.is_empty())
}

/// Opens the pane for `grant` with `open`, which the tests replace.
pub fn open_settings(grant: &str, open: impl FnOnce(&str) -> std::io::Result<()>) -> Value {
    let Some(g) = permissions::grant(grant) else {
        let names: Vec<&str> = GRANTS.iter().map(|g| g.name).collect();
        return json!({ "error": format!("no grant named {grant}; use one of {}", names.join(", ")) });
    };
    if cfg!(not(target_os = "macos")) {
        return json!({ "error": "System Settings exists only on macOS" });
    }
    let url = permissions::settings_url(g.pane);
    match open(&url) {
        Ok(()) => json!({ "opened": url, "grant": g.name }),
        Err(e) => json!({ "error": format!("cannot open {url}: {e}") }),
    }
}

/// Runs `open <url>`.
pub fn open_url(url: &str) -> std::io::Result<()> {
    let status = std::process::Command::new("open").arg(url).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("open exited with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bricolo_providers::catalog;

    fn facts() -> Facts {
        Facts {
            capabilities: catalog::capabilities(),
            held: Permissions {
                accessibility: Some(true),
                post_events: Some(true),
                input_monitoring: Some(false),
                microphone: Some(true),
                camera: Some(false),
                screen_recording: Some(false),
            },
            not_asked: vec!["camera"],
            holder: "bricolo node".into(),
            plugin_grants: BTreeMap::from([
                (
                    "dictation".into(),
                    vec!["mic".into(), "disk:~/Notes".into()],
                ),
                (
                    "window-tiler".into(),
                    vec!["window".into(), "hotkey".into()],
                ),
            ]),
            scripts: vec!["speak".into()],
        }
    }

    fn capability<'a>(answer: &'a Value, name: &str) -> &'a Value {
        answer["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()
    }

    #[test]
    fn lists_who_may_call_each_capability() {
        let a = answer(&facts());
        assert_eq!(capability(&a, "mic")["plugins"], json!(["dictation"]));
        assert_eq!(capability(&a, "disk")["plugins"], json!(["dictation"]));
        assert_eq!(capability(&a, "window")["plugins"], json!(["window-tiler"]));
        assert_eq!(capability(&a, "clipboard")["plugins"], json!([]));
        assert_eq!(capability(&a, "mic")["needs"], "microphone");
        assert_eq!(a["scripts"], json!(["speak"]));
        assert_eq!(a["holder"], "bricolo node");
    }

    #[test]
    fn names_the_state_of_every_grant() {
        let a = answer(&facts());
        let states: BTreeMap<String, String> = a["grants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| {
                (
                    g["name"].as_str().unwrap().into(),
                    g["state"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert_eq!(states.len(), GRANTS.len());
        assert_eq!(states["accessibility"], "granted");
        assert_eq!(states["input_monitoring"], "missing");
        assert_eq!(states["camera"], "not-asked");
        assert_eq!(states["screen_recording"], "missing");
        let off_mac = answer(&Facts {
            held: Permissions::default(),
            ..facts()
        });
        assert!(
            off_mac["grants"]
                .as_array()
                .unwrap()
                .iter()
                .all(|g| g["state"] == "n/a")
        );
    }

    #[test]
    fn the_holder_is_the_bundle_else_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("bricolo.app/Contents/Helpers/bricolo.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<dict>\n\t<key>CFBundleIdentifier</key>\n\t<string>x</string>\n\t<key>CFBundleName</key>\n\t<string>bricolo node</string>\n</dict>",
        )
        .unwrap();
        let exe = app.join("Contents/MacOS/bricolo");
        assert_eq!(holder(&exe, Some("ghostty")), "bricolo node");
        let bare = dir.path().join("other.app/Contents/MacOS/bricolo");
        assert_eq!(holder(&bare, None), "other");
        let dev = Path::new("/repo/target/debug/bricolo");
        assert_eq!(holder(dev, Some("ghostty")), "your terminal (ghostty)");
        assert_eq!(
            holder(dev, None),
            "the bricolo program (/repo/target/debug/bricolo)"
        );
    }

    #[test]
    fn open_settings_opens_the_grant_pane() {
        let mut opened = None;
        let a = open_settings("microphone", |url| {
            opened = Some(url.to_string());
            Ok(())
        });
        if cfg!(target_os = "macos") {
            assert_eq!(
                opened.as_deref(),
                Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            );
            assert_eq!(a["grant"], "microphone");
        } else {
            assert!(opened.is_none());
            assert!(a["error"].is_string());
        }
    }

    #[test]
    fn open_settings_refuses_an_unknown_grant() {
        let a = open_settings("bluetooth", |_| panic!("must not open"));
        assert!(
            a["error"]
                .as_str()
                .unwrap()
                .contains("no grant named bluetooth")
        );
    }
}
