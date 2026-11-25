//! `plugin.toml`: what a plugin is, what it imports, which extra grants it
//! asks for, and its default settings.
//!
//! A plugin that imports `shell` or `http` lists what it may reach:
//!
//! ```toml
//! [shell]
//! allow = ["/usr/bin/say"]     # exact absolute paths
//! env = ["VOICE"]              # variable names it may set
//!
//! [http]
//! allow = ["api.example.com:443", "127.0.0.1:8765"]
//! ```
//!
//! Each entry is a grant you approve at install, shown and recorded as
//! `shell:<path>`, `shell:env:<NAME>` and `http:<host:port>`.

use std::collections::BTreeMap;
use std::path::Path;

use bricolo_providers::CAPABILITIES;
use bricolo_providers::http::valid_allow_entry;
use bricolo_providers::shell::{ShellGrant, is_host_env, valid_env_name, valid_program};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const STDLIB_PACKAGE: &str = "bricolo:stdlib";
pub const STDLIB_VERSION: &str = "0.1.0";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Standard library interfaces (`"log"`), or another plugin's
    /// interface as `"<plugin>/<interface>"`.
    #[serde(default)]
    pub imports: Vec<String>,
    /// Grants beyond the imports themselves, such as `"disk:~/Notes"` or
    /// `"disk:read:~/Documents"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    /// The built component, relative to the source folder. Installed
    /// plugins don't have it: they sit next to `plugin.wasm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<String>,
    /// Default settings, passed to `init` as JSON after `node.toml`
    /// overrides.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub settings: toml::Table,
    /// How the hub's Settings tab shows each setting: a label, help, a
    /// list of choices. Optional; the hub infers the rest from the default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings_ui: BTreeMap<String, SettingUi>,
    /// Programs `shell.run` may start and the variables it may set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellSection>,
    /// Servers `http.send` may reach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpSection>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShellSection {
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HttpSection {
    pub allow: Vec<String>,
}

/// `[settings_ui.<key>]`: how the hub shows one setting.
///
/// ```toml
/// [settings_ui.voice]
/// label = "Voice"
/// help = "Used from the next call."
/// choices = ["alba", "marius"]
/// ```
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SettingUi {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// The values to pick from. With a list default, several may be picked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Shown under Advanced. Paths and URLs are advanced unless this says
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced: Option<bool>,
    /// Not shown at all, such as a chord the Shortcuts tab already holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Self::parse(&text).map_err(|e| Error::parse(path, e))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Manifest = toml::from_str(text).map_err(|e| e.message().to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name) {
            return Err(format!(
                "name {:?} must be lowercase letters, digits and dashes",
                self.name
            ));
        }
        if self.version.trim().is_empty() {
            return Err("version is empty".into());
        }
        for import in &self.imports {
            match import.split_once('/') {
                None if CAPABILITIES.contains(&import.as_str()) => {}
                None => {
                    return Err(format!(
                        "imports {import:?}, which is not a standard library interface ({})",
                        CAPABILITIES.join(", ")
                    ));
                }
                Some((plugin, iface)) if valid_name(plugin) && valid_name(iface) => {
                    if plugin == self.name {
                        return Err(format!("imports its own interface {import:?}"));
                    }
                }
                Some(_) => return Err(format!("import {import:?} must be <plugin>/<interface>")),
            }
        }
        let imports_disk = self.imports.iter().any(|i| i == "disk");
        for perm in &self.permissions {
            let Some(path) = perm
                .strip_prefix("disk:read:")
                .or_else(|| perm.strip_prefix("disk:"))
            else {
                return Err(format!(
                    "permission {perm:?} must be disk:<path> or disk:read:<path>"
                ));
            };
            if !(path.starts_with("~/")
                || path.starts_with('/')
                || std::path::Path::new(path).is_absolute())
            {
                return Err(format!(
                    "permission {perm:?}: the path must be absolute or start with ~/"
                ));
            }
            if !imports_disk {
                return Err(format!("permission {perm:?} needs \"disk\" in imports"));
            }
        }
        if imports_disk && self.permissions.is_empty() {
            return Err("imports \"disk\" but asks for no disk:<path> permission".into());
        }
        self.validate_settings_ui()?;
        self.validate_allow_lists()
    }

    /// Each `[settings_ui.<key>]` names a setting, and choices fit its
    /// default: a string picks one, a list of strings picks several.
    fn validate_settings_ui(&self) -> Result<(), String> {
        for (key, ui) in &self.settings_ui {
            let Some(default) = self.settings.get(key) else {
                return Err(format!("[settings_ui.{key}] names no key in [settings]"));
            };
            if ui.choices.is_empty() {
                continue;
            }
            let fits = match default {
                toml::Value::String(_) => true,
                toml::Value::Array(items) => items.iter().all(toml::Value::is_str),
                _ => false,
            };
            if !fits {
                return Err(format!(
                    "[settings_ui.{key}] has choices, but the default is not a string or a list of strings"
                ));
            }
        }
        Ok(())
    }

    fn validate_allow_lists(&self) -> Result<(), String> {
        let imports = |cap: &str| self.imports.iter().any(|i| i == cap);
        let sections = [
            ("shell", self.shell.as_ref().map(|s| &s.allow)),
            ("http", self.http.as_ref().map(|h| &h.allow)),
        ];
        for (cap, allow) in sections {
            match allow {
                Some(_) if !imports(cap) => {
                    return Err(format!("has a [{cap}] section but does not import {cap:?}"));
                }
                Some(list) if list.is_empty() => {
                    return Err(format!("[{cap}] allow is empty"));
                }
                None if imports(cap) => {
                    return Err(format!("imports {cap:?} but has no [{cap}] allow list"));
                }
                _ => {}
            }
        }
        if let Some(shell) = &self.shell {
            if let Some(p) = shell.allow.iter().find(|p| !valid_program(p)) {
                return Err(format!(
                    "[shell] allow entry {p:?} must be an absolute path without `..`"
                ));
            }
            for name in &shell.env {
                if is_host_env(name) {
                    return Err(format!(
                        "[shell] env entry {name:?} is set by the node and cannot be listed"
                    ));
                }
                if !valid_env_name(name) {
                    return Err(format!(
                        "[shell] env entry {name:?} must be letters, digits and underscores, \
                         not starting with a digit"
                    ));
                }
            }
        }
        if let Some(e) = self
            .http
            .iter()
            .flat_map(|h| &h.allow)
            .find(|e| !valid_allow_entry(e))
        {
            return Err(format!(
                "[http] allow entry {e:?} must be a lowercase host:port, like \"127.0.0.1:8765\""
            ));
        }
        Ok(())
    }

    /// What `[shell]` lets this plugin run.
    pub fn shell_grant(&self) -> ShellGrant {
        self.shell
            .as_ref()
            .map(|s| ShellGrant {
                programs: s.allow.clone(),
                env: s.env.clone(),
            })
            .unwrap_or_default()
    }

    /// What `[http]` lets this plugin reach.
    pub fn http_allow(&self) -> Vec<String> {
        self.http
            .as_ref()
            .map(|h| h.allow.clone())
            .unwrap_or_default()
    }

    /// Standard library interfaces this plugin imports.
    pub fn stdlib_imports(&self) -> impl Iterator<Item = &str> {
        self.imports
            .iter()
            .filter(|i| !i.contains('/'))
            .map(String::as_str)
    }

    /// Plugins whose interfaces this plugin imports.
    pub fn dependencies(&self) -> Vec<&str> {
        let mut deps: Vec<&str> = self
            .imports
            .iter()
            .filter_map(|i| i.split_once('/').map(|(p, _)| p))
            .collect();
        deps.sort();
        deps.dedup();
        deps
    }

    /// What install shows and `permissions.toml` records: each standard
    /// library import, each extra permission, then the allow lists.
    pub fn requested(&self) -> Vec<String> {
        let shell = self.shell.iter().flat_map(|s| {
            let programs = s.allow.iter().map(|p| format!("shell:{p}"));
            programs.chain(s.env.iter().map(|n| format!("shell:env:{n}")))
        });
        let http = self.http_allow().into_iter().map(|e| format!("http:{e}"));
        self.stdlib_imports()
            .map(str::to_string)
            .chain(self.permissions.iter().cloned())
            .chain(shell)
            .chain(http)
            .collect()
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Decides whether a component import is allowed. WASI is allowed because
/// the node links it with only the plugin's data folder.
pub fn check_import(name: &str, manifest: &Manifest) -> Result<(), String> {
    if name.starts_with("wasi:") {
        return Ok(());
    }
    let Some(rest) = name
        .strip_prefix(STDLIB_PACKAGE)
        .and_then(|r| r.strip_prefix('/'))
    else {
        return Err(format!(
            "imports {name}; imports from other plugins are not supported yet"
        ));
    };
    let (iface, version) = rest.split_once('@').unwrap_or((rest, ""));
    if version != STDLIB_VERSION {
        return Err(format!(
            "imports {name}, but this node provides {STDLIB_PACKAGE}@{STDLIB_VERSION}"
        ));
    }
    if iface == "types" || manifest.stdlib_imports().any(|i| i == iface) {
        Ok(())
    } else {
        Err(format!(
            "imports {iface}, which plugin.toml does not list in imports"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> Result<Manifest, String> {
        Manifest::parse(&format!("name = \"demo\"\nversion = \"0.1.0\"\n{text}"))
    }

    #[test]
    fn parses_imports_permissions_and_settings() {
        let m = manifest(
            r#"
imports = ["log", "disk", "voice/inbox"]
permissions = ["disk:~/Notes", "disk:read:/tmp"]
[settings]
greeting = "hi"
"#,
        )
        .unwrap();
        assert_eq!(
            m.requested(),
            ["log", "disk", "disk:~/Notes", "disk:read:/tmp"]
        );
        assert_eq!(m.dependencies(), ["voice"]);
        assert_eq!(m.settings["greeting"].as_str(), Some("hi"));
    }

    #[test]
    fn settings_ui_must_name_a_setting_and_fit_its_default() {
        let m = manifest(
            "[settings]\nvoice = \"alba\"\ncities = [\"Tokyo\"]\n\
             [settings_ui.voice]\nlabel = \"Voice\"\nchoices = [\"alba\", \"marius\"]\n\
             [settings_ui.cities]\nchoices = [\"Tokyo\", \"Paris\"]",
        )
        .unwrap();
        assert_eq!(m.settings_ui["voice"].label.as_deref(), Some("Voice"));
        let err = manifest("[settings]\na = 1\n[settings_ui.b]\nlabel = \"B\"").unwrap_err();
        assert!(err.contains("names no key"), "{err}");
        let err = manifest("[settings]\na = 1\n[settings_ui.a]\nchoices = [\"1\"]").unwrap_err();
        assert!(err.contains("not a string"), "{err}");
        assert!(manifest("[settings]\na = 1\n[settings_ui.a]\ncolour = 1").is_err());
    }

    #[test]
    fn rejects_bad_names_imports_and_permissions() {
        assert!(Manifest::parse("name = \"Demo\"\nversion = \"1\"").is_err());
        assert!(manifest("imports = [\"teleport\"]").is_err());
        assert!(manifest("imports = [\"demo/x\"]").is_err());
        assert!(
            manifest("imports = [\"disk\"]")
                .unwrap_err()
                .contains("no disk:<path>")
        );
        let err = manifest("imports = [\"log\"]\npermissions = [\"disk:~/x\"]").unwrap_err();
        assert!(err.contains("needs \"disk\""), "{err}");
        assert!(manifest("imports = [\"disk\"]\npermissions = [\"disk:x\"]").is_err());
        assert!(manifest("imports = [\"disk\"]\npermissions = [\"net:x\"]").is_err());
        assert!(manifest("colour = 1").is_err());
    }

    /// An absolute program path on this OS, in the form the manifest lists it.
    #[cfg(not(windows))]
    const PROGRAM: &str = "/usr/bin/say";
    #[cfg(windows)]
    const PROGRAM: &str = r"C:\Windows\System32\say.exe";

    #[test]
    fn allow_lists_become_grants() {
        let m = manifest(&format!(
            r#"
imports = ["log", "shell", "http"]
[shell]
allow = ['{PROGRAM}']
env = ["VOICE"]
[http]
allow = ["127.0.0.1:8765"]
"#
        ))
        .unwrap();
        assert_eq!(
            m.requested(),
            [
                "log".to_string(),
                "shell".into(),
                "http".into(),
                format!("shell:{PROGRAM}"),
                "shell:env:VOICE".into(),
                "http:127.0.0.1:8765".into()
            ]
        );
        assert_eq!(m.shell_grant().programs, [PROGRAM]);
        assert_eq!(m.http_allow(), ["127.0.0.1:8765"]);
        // The sections survive the round trip install writes.
        let text = toml::to_string_pretty(&m).unwrap();
        assert_eq!(Manifest::parse(&text).unwrap(), m);
    }

    #[test]
    fn rejects_bad_allow_lists() {
        let err = |text: &str| manifest(text).unwrap_err();
        assert!(err("imports = [\"shell\"]").contains("no [shell] allow"));
        assert!(err("[http]\nallow = [\"a:1\"]").contains("does not import"));
        assert!(err("imports = [\"shell\"]\n[shell]\nallow = []").contains("empty"));
        assert!(err("imports = [\"shell\"]\n[shell]\nallow = [\"say\"]").contains("absolute"));
        assert!(
            err(&format!(
                "imports = [\"shell\"]\n[shell]\nallow = ['{PROGRAM}']\nenv = [\"PATH\"]"
            ))
            .contains("set by the node")
        );
        assert!(
            err("imports = [\"http\"]\n[http]\nallow = [\"example.com\"]").contains("host:port")
        );
    }

    #[test]
    fn imports_must_be_listed_and_versioned() {
        let m = manifest("imports = [\"log\", \"kv\"]").unwrap();
        assert!(check_import("bricolo:stdlib/log@0.1.0", &m).is_ok());
        assert!(check_import("bricolo:stdlib/types@0.1.0", &m).is_ok());
        assert!(check_import("wasi:cli/environment@0.2.6", &m).is_ok());
        let err = check_import("bricolo:stdlib/disk@0.1.0", &m).unwrap_err();
        assert!(err.contains("does not list"), "{err}");
        assert!(check_import("bricolo:stdlib/log@0.2.0", &m).is_err());
        assert!(check_import("acme:x/y@1.0.0", &m).is_err());
    }
}
