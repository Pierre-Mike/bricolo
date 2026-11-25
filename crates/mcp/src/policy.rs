//! Which methods on which nodes the agent may see and call.
//!
//! A pattern is `node`, `node.capability` or `node.capability.method`,
//! and any part may be `*`: `*` allows everything, `studio` every method
//! on the node named studio, `*.clipboard` the clipboard on every node,
//! and `laptop.disk.write` one method. Method names may use the WIT's
//! dashes or Rust's underscores. A method is allowed when an `allow`
//! pattern matches and no `deny` pattern does.
//!
//! The policy only narrows what the gateway offers. Each node still
//! applies its own checks: a peer's allow list in its peers.toml, and the
//! gateway's disk grants on this node.

/// The allow and deny patterns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl Default for Policy {
    /// Everything allowed.
    fn default() -> Self {
        Policy::new(vec!["*".into()], vec![])
    }
}

impl Policy {
    pub fn new(allow: Vec<String>, deny: Vec<String>) -> Self {
        Policy { allow, deny }
    }

    pub fn allows(&self, node: &str, capability: &str, method: &str) -> bool {
        let target = [node, capability, &method.replace('_', "-")];
        let hit = |p: &String| matches(p, &target);
        self.allow.iter().any(hit) && !self.deny.iter().any(hit)
    }
}

fn matches(pattern: &str, target: &[&str; 3]) -> bool {
    let parts: Vec<String> = pattern.split('.').map(|p| p.replace('_', "-")).collect();
    if parts.len() > 3 {
        return false;
    }
    parts
        .iter()
        .zip(target)
        .all(|(p, t)| p == "*" || p == &t.replace('_', "-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(allow: &[&str], deny: &[&str]) -> Policy {
        Policy::new(
            allow.iter().map(|s| s.to_string()).collect(),
            deny.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn patterns_match_by_prefix_and_wildcard() {
        let all = Policy::default();
        assert!(all.allows("studio", "disk", "write"));

        let p = policy(
            &["laptop", "*.clipboard", "studio.speech.transcribe_file"],
            &[],
        );
        assert!(p.allows("laptop", "disk", "write"));
        assert!(p.allows("studio", "clipboard", "get"));
        assert!(p.allows("studio", "speech", "transcribe-file"));
        assert!(!p.allows("studio", "speech", "transcribe"));
        assert!(!p.allows("studio", "disk", "read"));
        assert!(!policy(&["a.b.c.d"], &[]).allows("a", "b", "c"));
    }

    #[test]
    fn deny_wins() {
        let p = policy(&["*"], &["*.disk.write", "phone"]);
        assert!(p.allows("laptop", "disk", "read"));
        assert!(!p.allows("laptop", "disk", "write"));
        assert!(!p.allows("phone", "tts", "speak"));
    }
}
