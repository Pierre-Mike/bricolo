//! Where an install comes from, as `bricolo install` takes it and
//! `plugins.lock` records it: `github:owner/repo[@tag]`,
//! `peer:<peer>/<sha256>`, `path:<path>`, or a plain path.

use std::fmt;
use std::path::PathBuf;

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Path(PathBuf),
    GitHub(GitHubRef),
    /// A plugin a peer has installed, by the sha256 of its `plugin.wasm`.
    /// Only a running node can fetch it, over the mesh.
    Peer {
        peer: String,
        sha256: String,
    },
}

/// A GitHub repository and, once resolved, a release tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRef {
    pub owner: String,
    pub repo: String,
    /// None means the latest release.
    pub tag: Option<String>,
}

impl GitHubRef {
    pub fn at(&self, tag: &str) -> Self {
        Self {
            tag: Some(tag.to_string()),
            ..self.clone()
        }
    }
}

impl Source {
    pub fn parse(text: &str) -> Result<Self> {
        if let Some(rest) = text.strip_prefix("peer:") {
            return parse_peer(rest).ok_or_else(|| {
                Error::Invalid(format!(
                    "{text:?} must look like peer:<name>/<sha256 of its plugin.wasm>"
                ))
            });
        }
        if let Some(rest) = text.strip_prefix("github:") {
            return parse_github(rest).map(Source::GitHub).ok_or_else(|| {
                Error::Invalid(format!(
                    "{text:?} must look like github:owner/repo or github:owner/repo@v1.2.0"
                ))
            });
        }
        let path = text.strip_prefix("path:").unwrap_or(text);
        if path.is_empty() {
            return Err(Error::Invalid("the path is empty".into()));
        }
        Ok(Source::Path(PathBuf::from(path)))
    }
}

fn parse_peer(text: &str) -> Option<Source> {
    let (peer, sha256) = text.split_once('/')?;
    let peer_ok = !peer.is_empty()
        && peer
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let sha_ok = sha256.len() == 64
        && sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (peer_ok && sha_ok).then(|| Source::Peer {
        peer: peer.into(),
        sha256: sha256.into(),
    })
}

fn parse_github(text: &str) -> Option<GitHubRef> {
    let (repo_part, tag) = match text.split_once('@') {
        Some((r, t)) => (r, Some(t)),
        None => (text, None),
    };
    let (owner, repo) = repo_part.split_once('/')?;
    let name_ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    let tag_ok = |t: &str| {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b))
    };
    if !name_ok(owner) || !name_ok(repo) || !tag.is_none_or(tag_ok) {
        return None;
    }
    Some(GitHubRef {
        owner: owner.into(),
        repo: repo.into(),
        tag: tag.map(String::from),
    })
}

impl fmt::Display for GitHubRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "github:{}/{}", self.owner, self.repo)?;
        match &self.tag {
            Some(tag) => write!(f, "@{tag}"),
            None => Ok(()),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Path(path) => write!(f, "path:{}", path.display()),
            Source::GitHub(gh) => gh.fmt(f),
            Source::Peer { peer, sha256 } => write!(f, "peer:{peer}/{sha256}"),
        }
    }
}

/// Whether release tag `latest` is newer than `current`. Tags compare as
/// dotted numbers after an optional `v` (`v1.10.0` > `v1.9.2`); a tag that
/// isn't one counts as newer whenever it differs.
pub fn newer(latest: &str, current: &str) -> bool {
    fn numbers(tag: &str) -> Option<Vec<u64>> {
        let tag = tag.strip_prefix('v').unwrap_or(tag);
        let core = tag.split(['-', '+']).next()?;
        core.split('.').map(|n| n.parse().ok()).collect()
    }
    match (numbers(latest), numbers(current)) {
        (Some(a), Some(b)) => a > b,
        _ => latest != current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_sources() {
        let gh = Source::parse("github:me/clip-tool@v1.2.0").unwrap();
        assert_eq!(
            gh,
            Source::GitHub(GitHubRef {
                owner: "me".into(),
                repo: "clip-tool".into(),
                tag: Some("v1.2.0".into()),
            })
        );
        assert_eq!(gh.to_string(), "github:me/clip-tool@v1.2.0");
        let latest = Source::parse("github:me/clip-tool").unwrap();
        assert_eq!(latest.to_string(), "github:me/clip-tool");
        for bad in [
            "github:me",
            "github:/x",
            "github:me/x@",
            "github:me/../x",
            "github:me/x@a/b",
            "github:me/x y",
        ] {
            assert!(Source::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            Source::parse("path:/a/b").unwrap(),
            Source::Path("/a/b".into())
        );
        assert_eq!(Source::parse("./x").unwrap(), Source::Path("./x".into()));
        let sha = "0a".repeat(32);
        let peer = Source::parse(&format!("peer:mac-studio/{sha}")).unwrap();
        assert_eq!(
            peer,
            Source::Peer {
                peer: "mac-studio".into(),
                sha256: sha.clone()
            }
        );
        assert_eq!(peer.to_string(), format!("peer:mac-studio/{sha}"));
        for bad in [
            "peer:mac".to_string(),
            format!("peer:/{sha}"),
            "peer:mac/abc".to_string(),
            format!("peer:mac/{}", "0A".repeat(32)),
            format!("peer:a b/{sha}"),
        ] {
            assert!(Source::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn compares_tags() {
        assert!(newer("v1.10.0", "v1.9.2"));
        assert!(newer("2", "v1.9"));
        assert!(!newer("v1.2.0", "v1.2.0"));
        assert!(!newer("v1.1.0", "v1.2.0"));
        assert!(newer("nightly-2", "nightly-1"));
        assert!(!newer("nightly", "nightly"));
    }
}
