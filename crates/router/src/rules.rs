use std::collections::BTreeMap;
use std::path::Path;

use bricolo_providers::{CAPABILITIES, ROUTABLE};
use serde::Deserialize;
use thiserror::Error;

/// The target name for this node.
pub const LOCAL: &str = "local";

#[derive(Debug, Error)]
pub enum RulesError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid rules.toml: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid rules.toml: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub routes: BTreeMap<String, Vec<String>>,
}

fn default_timeout() -> u64 {
    30
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            timeout_secs: default_timeout(),
            routes: BTreeMap::new(),
        }
    }
}

impl Rules {
    pub fn parse(text: &str) -> Result<Self, RulesError> {
        let rules: Rules = toml::from_str(text)?;
        rules.validate()?;
        Ok(rules)
    }

    /// Reads `path`; a missing file means every call runs locally.
    pub fn load(path: &Path) -> Result<Self, RulesError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(RulesError::Read {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    pub fn validate(&self) -> Result<(), RulesError> {
        if self.timeout_secs == 0 {
            return Err(RulesError::Invalid(
                "timeout_secs must be at least 1".into(),
            ));
        }
        for (cap, targets) in &self.routes {
            if !ROUTABLE.contains(&cap.as_str()) {
                let why = if CAPABILITIES.contains(&cap.as_str()) {
                    format!("{cap} always runs on this node and cannot be routed")
                } else {
                    format!("unknown capability {cap:?}")
                };
                return Err(RulesError::Invalid(why));
            }
            if targets.is_empty() {
                return Err(RulesError::Invalid(format!("{cap} has no targets")));
            }
            if let Some(t) = targets.iter().find(|t| t.is_empty()) {
                return Err(RulesError::Invalid(format!(
                    "{cap} has an empty target {t:?}"
                )));
            }
        }
        Ok(())
    }

    /// The targets for `capability`, in order. Unlisted means local.
    pub fn targets(&self, capability: &str) -> Vec<String> {
        self.routes
            .get(capability)
            .cloned()
            .unwrap_or_else(|| vec![LOCAL.to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_routes_and_defaults_to_local() {
        let rules = Rules::parse(
            r#"
timeout_secs = 5
[routes]
speech = ["desktop", "local"]
"#,
        )
        .unwrap();
        assert_eq!(rules.timeout_secs, 5);
        assert_eq!(rules.targets("speech"), ["desktop", "local"]);
        assert_eq!(rules.targets("tts"), ["local"]);
    }

    #[test]
    fn empty_file_is_all_local() {
        let rules = Rules::parse("").unwrap();
        assert_eq!(rules, Rules::default());
        assert_eq!(rules.targets("disk"), ["local"]);
    }

    #[test]
    fn missing_file_is_all_local() {
        let rules = Rules::load(Path::new("/nonexistent/rules.toml")).unwrap();
        assert_eq!(rules, Rules::default());
    }

    #[test]
    fn rejects_local_only_unknown_and_empty_routes() {
        let err = Rules::parse("[routes]\nkv = [\"desktop\"]").unwrap_err();
        assert!(
            err.to_string().contains("always runs on this node"),
            "{err}"
        );
        let err = Rules::parse("[routes]\nmic2 = [\"local\"]").unwrap_err();
        assert!(err.to_string().contains("unknown capability"), "{err}");
        let err = Rules::parse("[routes]\ntts = []").unwrap_err();
        assert!(err.to_string().contains("no targets"), "{err}");
        assert!(Rules::parse("timeout_secs = 0").is_err());
        assert!(Rules::parse("colour = 1").is_err());
    }
}
