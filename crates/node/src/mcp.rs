//! `bricolo mcp`: the command line side of the MCP gateway that `bricolo run`
//! hosts (see `gateway.rs`).
//!
//! - `bricolo mcp` pipes standard input and output to the running node's
//!   gateway over the control socket, for MCP clients that start a
//!   command, like Claude Code and Claude Desktop.
//! - `bricolo mcp token` prints the HTTP gateway's token.
//! - `bricolo mcp url` prints the addresses an agent can use.
//! - `bricolo mcp funnel` publishes the HTTP gateway with Tailscale Funnel.
//! - `bricolo mcp connect claude-code|claude-desktop` adds the gateway to a
//!   client's MCP settings.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use bricolo_mesh::StatusSource;
use bricolo_runtime::Paths;
use bricolo_runtime::node_config::NodeConfig;
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::control;
use crate::gateway::{McpConfig, token_path};

#[derive(Subcommand)]
pub enum McpCommand {
    /// Print the HTTP gateway's token, making one if there is none.
    Token {
        /// Replace the token with a new one. Restart the node after.
        #[arg(long)]
        rotate: bool,
    },
    /// Print the addresses an agent can reach the gateway at.
    Url,
    /// Publish the HTTP gateway on the internet with Tailscale Funnel, so
    /// a cloud agent like Claude can add it as a custom connector.
    Funnel {
        /// Stop publishing it.
        #[arg(long)]
        off: bool,
        /// The public HTTPS port. Funnel allows 443, 8443 and 10000; pick
        /// one no other Funnel or serve config on this machine uses.
        #[arg(long, default_value_t = 443)]
        port: u16,
        /// Print the commands instead of running them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Add the gateway to an MCP client's settings.
    Connect {
        client: Client,
        /// Print what would change instead of changing it.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
}

pub fn main(paths: &Paths, command: Option<McpCommand>) -> Result<ExitCode> {
    let runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("cannot start Tokio")
    };
    match command {
        None => runtime()?.block_on(bridge(paths)),
        Some(McpCommand::Token { rotate }) => {
            let path = token_path(paths);
            if rotate {
                let _ = std::fs::remove_file(&path);
            }
            println!("{}", bricolo_mcp::http::load_or_create_token(&path)?);
            Ok(ExitCode::SUCCESS)
        }
        Some(McpCommand::Url) => runtime()?.block_on(print_urls(paths)),
        Some(McpCommand::Funnel { off, port, dry_run }) => funnel(paths, off, port, dry_run),
        Some(McpCommand::Connect { client, dry_run }) => connect(client, dry_run),
    }
}

/// Copies standard input to an `mcp-session` on the control socket, and
/// the answers back to standard output.
async fn bridge(paths: &Paths) -> Result<ExitCode> {
    let (mut read, mut write) = match control::connect(paths).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!(
                "no node answers on {}: {e:#}; start it with `bricolo run` or `bricolo service install`",
                paths.socket().display()
            );
            return Ok(ExitCode::from(crate::NOT_RUNNING));
        }
    };
    write.write_all(b"{\"cmd\":\"mcp-session\"}\n").await?;
    let up = async {
        tokio::io::copy(&mut tokio::io::stdin(), &mut write).await?;
        write.shutdown().await
    };
    let down = async {
        let mut out = tokio::io::stdout();
        tokio::io::copy(&mut read, &mut out).await?;
        out.flush().await
    };
    tokio::pin!(down);
    // The client closing its input ends the session once the node has
    // answered what is in flight; the node closing ends it at once.
    tokio::select! {
        r = &mut down => r?,
        r = up => {
            r?;
            down.await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn config(paths: &Paths) -> Result<(NodeConfig, McpConfig)> {
    let node = NodeConfig::load(&paths.node_toml())?;
    let mcp = McpConfig::from_table(&node.mcp)?;
    Ok((node, mcp))
}

async fn print_urls(paths: &Paths) -> Result<ExitCode> {
    let (node, mcp) = config(paths)?;
    println!("stdio:  bricolo mcp   (node {})", node.name);
    let Some(listen) = mcp.listen else {
        println!("http:   off; set [mcp] listen = \"127.0.0.1:8790\" in node.toml");
        return Ok(ExitCode::SUCCESS);
    };
    let token = bricolo_mcp::http::load_or_create_token(&token_path(paths))?;
    println!("http:   http://{listen}/mcp   (header Authorization: Bearer <bricolo mcp token>)");
    if let Ok(name) = tailnet_name().await {
        println!(
            "funnel: {}   (after `bricolo mcp funnel`; add :8443 or :10000 if you used --port)",
            funnel_url(&name, 443, &token)
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// This machine's name on the tailnet, like `mac.tail1234.ts.net`.
async fn tailnet_name() -> Result<String> {
    let StatusSource::Command(cli) = StatusSource::default_command() else {
        bail!("no tailscale CLI");
    };
    let out = tokio::process::Command::new(cli)
        .args(["status", "--json"])
        .output()
        .await
        .context("cannot run tailscale")?;
    let status: Value = serde_json::from_slice(&out.stdout).context("bad tailscale status")?;
    let name = status["Self"]["DNSName"]
        .as_str()
        .unwrap_or_default()
        .trim_end_matches('.');
    if name.is_empty() {
        bail!("tailscale is not up");
    }
    Ok(name.to_string())
}

/// The `tailscale` arguments that publish (or stop publishing) the local
/// `port` on the public HTTPS port `public`.
pub fn funnel_args(port: u16, public: u16, off: bool) -> Vec<String> {
    let https = format!("--https={public}");
    if off {
        vec!["funnel".into(), https, "off".into()]
    } else {
        vec!["funnel".into(), "--bg".into(), https, port.to_string()]
    }
}

/// The public URL, without the port when it is HTTPS's own.
pub fn funnel_url(name: &str, public: u16, token: &str) -> String {
    match public {
        443 => format!("https://{name}/mcp/{token}"),
        p => format!("https://{name}:{p}/mcp/{token}"),
    }
}

fn funnel(paths: &Paths, off: bool, public: u16, dry_run: bool) -> Result<ExitCode> {
    let (_, mcp) = config(paths)?;
    let Some(listen) = mcp.listen else {
        bail!(
            "the HTTP gateway is off; set [mcp] listen = \"127.0.0.1:8790\" in node.toml and restart the node"
        );
    };
    if !listen.ip().is_loopback() {
        eprintln!(
            "warning: [mcp] listen is {listen}; bind it to 127.0.0.1 so only the funnel reaches it"
        );
    }
    let StatusSource::Command(cli) = StatusSource::default_command() else {
        bail!("no tailscale CLI");
    };
    if ![443, 8443, 10000].contains(&public) {
        bail!("Funnel publishes only on 443, 8443 or 10000, not {public}");
    }
    let args = funnel_args(listen.port(), public, off);
    if dry_run {
        println!("{} {}", cli.display(), args.join(" "));
        return Ok(ExitCode::SUCCESS);
    }
    let status = std::process::Command::new(&cli)
        .args(&args)
        .status()
        .with_context(|| format!("cannot run {}", cli.display()))?;
    if !status.success() {
        bail!(
            "tailscale funnel failed; Funnel needs HTTPS certificates and the funnel attribute on your tailnet (see docs/mcp.md)"
        );
    }
    if !off {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let token = bricolo_mcp::http::load_or_create_token(&token_path(paths))?;
        match runtime.block_on(tailnet_name()) {
            Ok(name) => println!(
                "published. Add this URL in Claude as a custom connector:\n{}\nAnyone with it can use the gateway: keep it secret, and rotate it with `bricolo mcp token --rotate`.",
                funnel_url(&name, public, &token)
            ),
            Err(e) => println!("published, but cannot read the tailnet name: {e:#}"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Where Claude Desktop keeps its MCP settings.
pub fn desktop_config_path(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude/claude_desktop_config.json")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Claude/claude_desktop_config.json")
    } else {
        home.join(".config/Claude/claude_desktop_config.json")
    }
}

/// Claude Desktop's settings with `mcpServers.bricolo` set to run `exe mcp`,
/// keeping everything else.
pub fn with_desktop_server(existing: Option<&str>, exe: &Path) -> Result<String> {
    let mut config: Value = match existing {
        Some(text) if !text.trim().is_empty() => {
            serde_json::from_str(text).context("the Claude Desktop config is not valid JSON")?
        }
        _ => json!({}),
    };
    let Some(root) = config.as_object_mut() else {
        bail!("the Claude Desktop config is not a JSON object");
    };
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        bail!("mcpServers in the Claude Desktop config is not an object");
    };
    servers.insert(
        "bricolo".into(),
        json!({ "command": exe.to_string_lossy(), "args": ["mcp"] }),
    );
    Ok(serde_json::to_string_pretty(&config)? + "\n")
}

/// The `claude` arguments that add the gateway for every project.
pub fn claude_code_args(exe: &Path) -> Vec<String> {
    vec![
        "mcp".into(),
        "add".into(),
        "--scope".into(),
        "user".into(),
        "bricolo".into(),
        "--".into(),
        exe.to_string_lossy().into_owned(),
        "mcp".into(),
    ]
}

fn connect(client: Client, dry_run: bool) -> Result<ExitCode> {
    let exe = std::env::current_exe().context("cannot find the bricolo binary")?;
    if dry_run {
        match client {
            Client::ClaudeCode => println!("claude {}", claude_code_args(&exe).join(" ")),
            Client::ClaudeDesktop => {
                let home = bricolo_runtime::paths::home_dir().context("no home folder")?;
                let path = desktop_config_path(&home);
                let existing = std::fs::read_to_string(&path).ok();
                println!(
                    "{}:\n{}",
                    path.display(),
                    with_desktop_server(existing.as_deref(), &exe)?
                );
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    println!("{}", connect_quietly(client)?);
    Ok(ExitCode::SUCCESS)
}

/// Adds the gateway to `client` and says what it did. The hub's Connect
/// buttons call this through the control socket.
pub fn connect_quietly(client: Client) -> Result<String> {
    let exe = std::env::current_exe().context("cannot find the bricolo binary")?;
    match client {
        Client::ClaudeCode => {
            let args = claude_code_args(&exe);
            let out = std::process::Command::new("claude").args(&args).output();
            match out {
                Ok(o) if o.status.success() => {
                    Ok("Added bricolo to Claude Code. Restart open sessions to see it.".into())
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    if err.contains("already exists") {
                        Ok("Claude Code already has bricolo.".into())
                    } else {
                        bail!("`claude mcp add` failed: {}", err.trim())
                    }
                }
                Err(_) => bail!(
                    "no claude CLI on PATH; run this where it is: claude {}",
                    args.join(" ")
                ),
            }
        }
        Client::ClaudeDesktop => {
            let home = bricolo_runtime::paths::home_dir().context("no home folder")?;
            let path = desktop_config_path(&home);
            let existing = std::fs::read_to_string(&path).ok();
            let updated = with_desktop_server(existing.as_deref(), &exe)?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            if existing.is_some() {
                std::fs::copy(&path, path.with_extension("json.bak"))?;
            }
            std::fs::write(&path, updated)?;
            Ok(format!(
                "Added bricolo to {}. Restart Claude Desktop to see it.",
                path.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_config_keeps_other_servers() {
        let exe = Path::new("/Applications/bricolo");
        let fresh: Value = serde_json::from_str(&with_desktop_server(None, exe).unwrap()).unwrap();
        assert_eq!(fresh["mcpServers"]["bricolo"]["args"], json!(["mcp"]));

        let existing = r#"{"theme":"dark","mcpServers":{"other":{"command":"x"},"bricolo":{"command":"old"}}}"#;
        let merged: Value =
            serde_json::from_str(&with_desktop_server(Some(existing), exe).unwrap()).unwrap();
        assert_eq!(merged["theme"], "dark");
        assert_eq!(merged["mcpServers"]["other"]["command"], "x");
        assert_eq!(
            merged["mcpServers"]["bricolo"]["command"],
            "/Applications/bricolo"
        );

        assert!(with_desktop_server(Some("[1]"), exe).is_err());
        assert!(with_desktop_server(Some("{"), exe).is_err());
    }

    #[test]
    fn commands_for_claude_code_and_funnel() {
        let args = claude_code_args(Path::new("/usr/local/bin/bricolo"));
        assert_eq!(
            args.join(" "),
            "mcp add --scope user bricolo -- /usr/local/bin/bricolo mcp"
        );
        assert_eq!(
            funnel_args(8790, 443, false),
            ["funnel", "--bg", "--https=443", "8790"]
        );
        assert_eq!(
            funnel_args(8790, 10000, true),
            ["funnel", "--https=10000", "off"]
        );
        assert_eq!(funnel_url("m.ts.net", 443, "t"), "https://m.ts.net/mcp/t");
        assert_eq!(
            funnel_url("m.ts.net", 10000, "t"),
            "https://m.ts.net:10000/mcp/t"
        );
        assert!(desktop_config_path(Path::new("/h")).ends_with("claude_desktop_config.json"));
    }
}
