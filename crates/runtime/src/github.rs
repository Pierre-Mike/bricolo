//! GitHub releases, for `bricolo install github:owner/repo[@tag]` and
//! `bricolo update`.
//!
//! The client reads `GET /repos/{owner}/{repo}/releases/latest` or
//! `/releases/tags/{tag}`, then downloads assets through their API `url`
//! with `Accept: application/octet-stream`, which works for private
//! repositories too. `GITHUB_TOKEN`, when set, goes in the
//! `Authorization` header of requests to the API only; ureq drops it when
//! GitHub redirects the download to its storage host.
//!
//! `BRICOLO_GITHUB_API` replaces `https://api.github.com`, so tests can serve a
//! fake API from a local server.

use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::bundle::MAX_SIZE;
use crate::error::{Error, Result};
use crate::source::GitHubRef;

pub const API_ENV: &str = "BRICOLO_GITHUB_API";
pub const TOKEN_ENV: &str = "GITHUB_TOKEN";
pub const DEFAULT_API: &str = "https://api.github.com";

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    /// The API URL of the asset, not the browser download link.
    pub url: String,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

pub struct GitHub {
    api: String,
    token: Option<String>,
    agent: Agent,
}

impl GitHub {
    pub fn new(api: impl Into<String>, token: Option<String>) -> Self {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let agent = Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(300)))
            .http_status_as_error(false)
            .tls_config(tls)
            .build()
            .into();
        Self {
            api: api.into().trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            agent,
        }
    }

    /// `BRICOLO_GITHUB_API` (default `https://api.github.com`) and
    /// `GITHUB_TOKEN`.
    pub fn from_env() -> Self {
        let api = std::env::var(API_ENV).unwrap_or_else(|_| DEFAULT_API.into());
        Self::new(api, std::env::var(TOKEN_ENV).ok())
    }

    /// The release `repo.tag` names, or the latest one.
    pub fn release(&self, repo: &GitHubRef) -> Result<Release> {
        let which = match &repo.tag {
            Some(tag) => format!("tags/{tag}"),
            None => "latest".into(),
        };
        let url = format!(
            "{}/repos/{}/{}/releases/{which}",
            self.api, repo.owner, repo.repo
        );
        let body = self.get(&url, "application/vnd.github+json", 1 << 20, || {
            let what = match &repo.tag {
                Some(tag) => format!("no release {tag}"),
                None => "no published release".into(),
            };
            format!(
                "{}/{} has {what}, or the repository is private and {TOKEN_ENV} is not set",
                repo.owner, repo.repo
            )
        })?;
        serde_json::from_slice(&body)
            .map_err(|e| Error::Fetch(format!("{url}: unexpected answer: {e}")))
    }

    pub fn download(&self, asset: &Asset) -> Result<Vec<u8>> {
        self.get(&asset.url, "application/octet-stream", MAX_SIZE, || {
            format!("asset {} is gone", asset.name)
        })
    }

    fn get(
        &self,
        url: &str,
        accept: &str,
        limit: u64,
        not_found: impl FnOnce() -> String,
    ) -> Result<Vec<u8>> {
        let mut request = self
            .agent
            .get(url)
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", concat!("bricolo/", env!("CARGO_PKG_VERSION")));
        // Only the API gets the token, never a host an answer points to.
        if let Some(token) = &self.token
            && url.starts_with(&format!("{}/", self.api))
        {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let mut response = request
            .call()
            .map_err(|e| Error::Fetch(format!("{url}: {e}")))?;
        let status = response.status().as_u16();
        match status {
            200 => {}
            404 => return Err(Error::Fetch(not_found())),
            401 | 403 | 429 => {
                let hint = if self.token.is_some() {
                    format!("check that {TOKEN_ENV} is valid and can read the repository")
                } else {
                    format!("set {TOKEN_ENV} for private repositories and a higher rate limit")
                };
                return Err(Error::Fetch(format!(
                    "GitHub refused {url} ({status}); {hint}"
                )));
            }
            other => return Err(Error::Fetch(format!("{url}: status {other}"))),
        }
        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map_err(|e| Error::Fetch(format!("{url}: {e}")))
    }
}
