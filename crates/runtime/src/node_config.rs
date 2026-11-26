//! `node.toml`: this node's name, runtime limits, plugin overrides, the
//! sidecars, which servers `tts` and `speech` use, the microphone, the
//! camera, and where Claude Code sessions may work.
//!
//! ```toml
//! name = "mac"
//!
//! [runtime]
//! event_budget_ms = 500   # time one event may run, not counting awaited
//!                         # host calls, before it traps
//! init_budget_ms = 2000
//!
//! [plugins.hello]         # overrides hello's [settings] defaults
//! greeting = "hi"
//!
//! [sidecars.parakeet]     # see bricolo_providers::sidecar
//! command = "~/.local/bin/uv"
//! args = ["run", "--script", "/path/to/bricolo/apps/shared/sidecars/parakeet/server.py"]
//! port = 8765
//!
//! [speech]                # see bricolo_providers::speech
//! sidecar = "parakeet"
//!
//! [tts]                   # see bricolo_providers::tts
//! sidecar = "pocket-en"
//! voice = "alba"
//!
//! [mic]                   # see bricolo_providers::mic
//! device = "MacBook Pro Microphone"
//!
//! [camera]                # see bricolo_providers::camera
//! device = "0"
//!
//! [claude-code]           # see bricolo_providers::claude_code
//! dirs = ["~/Github"]
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use bricolo_providers::camera::CameraConfig;
use bricolo_providers::claude_code::ClaudeCodeConfig;
use bricolo_providers::mic::MicConfig;
use bricolo_providers::sidecar::SidecarConfig;
use bricolo_providers::speech::SpeechConfig;
use bricolo_providers::tts::TtsConfig;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::lock::load_toml;
use crate::manifest::Manifest;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub runtime: RuntimeLimits,
    #[serde(default)]
    pub plugins: BTreeMap<String, toml::Table>,
    #[serde(default)]
    pub sidecars: BTreeMap<String, SidecarConfig>,
    #[serde(default)]
    pub tts: TtsConfig,
    #[serde(default)]
    pub speech: SpeechConfig,
    #[serde(default)]
    pub mic: MicConfig,
    #[serde(default)]
    pub camera: CameraConfig,
    /// `[claude-code]`: the folders Claude Code sessions may work in.
    #[serde(default, rename = "claude-code")]
    pub claude_code: ClaudeCodeConfig,
    /// The `[mesh]` settings, read by the node through `bricolo-mesh`.
    #[serde(default)]
    pub mesh: toml::Table,
    /// The `[mcp]` settings, read by the node's MCP gateway.
    #[serde(default)]
    pub mcp: toml::Table,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLimits {
    #[serde(default = "default_event_budget")]
    pub event_budget_ms: u64,
    #[serde(default = "default_init_budget")]
    pub init_budget_ms: u64,
}

fn default_name() -> String {
    "local".into()
}
fn default_event_budget() -> u64 {
    500
}
fn default_init_budget() -> u64 {
    2000
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            event_budget_ms: default_event_budget(),
            init_budget_ms: default_init_budget(),
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            name: default_name(),
            runtime: RuntimeLimits::default(),
            plugins: BTreeMap::new(),
            sidecars: BTreeMap::new(),
            tts: TtsConfig::default(),
            speech: SpeechConfig::default(),
            mic: MicConfig::default(),
            camera: CameraConfig::default(),
            claude_code: ClaudeCodeConfig::default(),
            mesh: toml::Table::new(),
            mcp: toml::Table::new(),
        }
    }
}

impl NodeConfig {
    /// A missing file means defaults.
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = load_toml(path)?;
        config.validate().map_err(|e| Error::parse(path, e))?;
        Ok(config)
    }

    /// `[tts]` and `[speech]` name a configured sidecar or a URL, not both.
    pub fn validate(&self) -> Result<(), String> {
        let servers = [
            ("tts", &self.tts.sidecar, &self.tts.url),
            ("speech", &self.speech.sidecar, &self.speech.url),
        ];
        for (section, sidecar, url) in servers {
            if sidecar.is_some() && url.is_some() {
                return Err(format!("[{section}] sets both sidecar and url"));
            }
            if let Some(name) = sidecar
                && !self.sidecars.contains_key(name)
            {
                return Err(format!(
                    "[{section}] names sidecar {name:?}, but there is no [sidecars.{name}]"
                ));
            }
        }
        if let Some(player) = &self.tts.player
            && player.is_empty()
        {
            return Err("[tts] player is empty".into());
        }
        Ok(())
    }

    /// The JSON object `init` receives: the manifest's `[settings]`, with
    /// each key from `[plugins.<name>]` replacing the default.
    pub fn settings_json(&self, manifest: &Manifest) -> String {
        let mut settings = manifest.settings.clone();
        if let Some(overrides) = self.plugins.get(&manifest.name) {
            for (k, v) in overrides {
                settings.insert(k.clone(), v.clone());
            }
        }
        serde_json::to_string(&settings).unwrap_or_else(|_| "{}".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_replace_defaults_key_by_key() {
        let m = Manifest::parse(
            "name = \"hello\"\nversion = \"1\"\n[settings]\ngreeting = \"hello\"\ntick_ms = 50",
        )
        .unwrap();
        let node: NodeConfig = toml::from_str("[plugins.hello]\ngreeting = \"hi\"").unwrap();
        let json: serde_json::Value = serde_json::from_str(&node.settings_json(&m)).unwrap();
        assert_eq!(json, serde_json::json!({ "greeting": "hi", "tick_ms": 50 }));
        let json = NodeConfig::default().settings_json(&m);
        assert!(json.contains("\"greeting\":\"hello\""), "{json}");
    }

    #[test]
    fn defaults_and_unknown_keys() {
        let node: NodeConfig = toml::from_str("").unwrap();
        assert_eq!(node, NodeConfig::default());
        assert_eq!(node.runtime.event_budget_ms, 500);
        assert!(toml::from_str::<NodeConfig>("colour = 1").is_err());
    }

    #[test]
    fn reads_claude_code() {
        let node: NodeConfig = toml::from_str(
            "[claude-code]\ndirs = [\"~/Github\"]\npermission_mode = \"plan\"\nextra_args = [\"--model\", \"opus\"]",
        )
        .unwrap();
        assert_eq!(node.claude_code.dirs, ["~/Github"]);
        assert_eq!(node.claude_code.permission_mode.as_deref(), Some("plan"));
        assert_eq!(node.claude_code.extra_args, ["--model", "opus"]);
        assert_eq!(node.claude_code.command, None);
        assert!(toml::from_str::<NodeConfig>("[claude-code]\nfolders = []").is_err());
    }

    #[test]
    fn reads_sidecars_and_the_servers_that_use_them() {
        let node: NodeConfig = toml::from_str(
            r#"
[sidecars.parakeet]
command = "~/py"
args = ["server.py"]
port = 8765
env = { STT_PORT = "8765" }

[speech]
sidecar = "parakeet"

[tts]
url = "http://127.0.0.1:8791"
voice = "alba"
player = ["/usr/bin/afplay"]
[tts.profiles]
alerts = "marius"

[mic]
device = "MacBook Pro Microphone"
ffmpeg = "/opt/homebrew/bin/ffmpeg"
"#,
        )
        .unwrap();
        node.validate().unwrap();
        assert_eq!(node.mic.device.as_deref(), Some("MacBook Pro Microphone"));
        assert!(toml::from_str::<NodeConfig>("[mic]\ninput = \"x\"").is_err());
        assert_eq!(node.sidecars["parakeet"].port, 8765);
        assert_eq!(node.tts.profiles["alerts"], "marius");
        let bad: NodeConfig = toml::from_str("[speech]\nsidecar = \"nope\"").unwrap();
        assert!(bad.validate().unwrap_err().contains("no [sidecars.nope]"));
        let both: NodeConfig = toml::from_str(
            "[sidecars.a]\ncommand = \"x\"\nport = 1\n[tts]\nsidecar = \"a\"\nurl = \"http://x:1\"",
        )
        .unwrap();
        assert!(both.validate().is_err());
        assert!(toml::from_str::<NodeConfig>("[sidecars.a]\ncommand = \"x\"").is_err());
    }
}
