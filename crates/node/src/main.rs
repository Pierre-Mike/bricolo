//! `bricolo`: runs the node and manages its plugins.

mod control;
mod dev;
mod gateway;
mod library;
mod logs;
mod mcp;
mod mesh;
#[cfg(windows)]
mod pipe_security;
mod plugins;
mod scaffold;
mod service;
mod settings;
mod share;
mod sidecar;
mod skill;
mod state;
mod subscribe;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use bricolo_runtime::Paths;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

/// Exit code when a command needs a running node and none answers.
const NOT_RUNNING: u8 = 2;

#[derive(Parser)]
#[command(name = "bricolo", version, about = "bricolo node")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the node in the foreground until Ctrl+C or SIGTERM.
    Run,
    /// Print the running node's state as JSON.
    Status,
    /// Deliver `hotkey(<id>)` to a running plugin, as if its chord was
    /// pressed.
    Fire { plugin: String, id: String },
    /// Print the macOS permissions the running node holds: Accessibility
    /// for `window` and `keyboard`, Input Monitoring for the `fn` hotkey.
    Permissions {
        /// Ask macOS for what is missing (it lists the node in System
        /// Settings and prompts once) and open the settings to turn it on.
        #[arg(long)]
        request: bool,
        /// Print what this new process reads itself, without asking the
        /// node. The node runs it to see grants given after it started.
        #[arg(long, hide = true, conflicts_with = "request")]
        local: bool,
    },
    /// Create a plugin crate in <dir>/<name> from a template.
    New {
        #[arg(required_unless_present = "list")]
        name: Option<String>,
        /// The template; `--list` shows them.
        #[arg(long, short = 't', default_value = "plugin")]
        template: String,
        /// List the templates.
        #[arg(long)]
        list: bool,
        /// Parent folder for the new crate.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// The sidecar port, for the sidecar-client template.
        #[arg(long, default_value_t = scaffold::DEFAULT_SIDECAR_PORT)]
        port: u16,
    },
    /// Write a script to scripts/<name>.lua from a template.
    NewScript {
        #[arg(required_unless_present = "list")]
        name: Option<String>,
        /// The template; `--list` shows them.
        #[arg(long, short = 't', default_value = "starter")]
        template: String,
        /// List the templates.
        #[arg(long)]
        list: bool,
        /// The sidecar port, for the sidecar-client template.
        #[arg(long, default_value_t = scaffold::DEFAULT_SIDECAR_PORT)]
        port: u16,
    },
    /// Create a sidecar in <dir>/<name>: a local HTTP server, on
    /// 127.0.0.1, that `bricolo sidecar add` hands to the node.
    NewSidecar {
        #[arg(required_unless_present = "list")]
        name: Option<String>,
        /// python, node or rust; `--list` shows them.
        #[arg(long, short = 't', default_value = "python")]
        template: String,
        /// List the templates.
        #[arg(long)]
        list: bool,
        /// The port it listens on.
        #[arg(long, default_value_t = scaffold::DEFAULT_SIDECAR_PORT)]
        port: u16,
        /// Parent folder for the new sidecar.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Install a plugin or scripts: a built plugin folder, a .lua file, a
    /// folder of scripts, a bundle from `bricolo pack`, github:owner/repo[@tag],
    /// or peer:<peer>/<sha256>, a plugin a peer has installed.
    Install {
        #[arg(required_unless_present = "from_lock")]
        source: Option<String>,
        /// Reinstall everything plugins.lock lists, at the locked hashes.
        #[arg(long, conflicts_with = "source")]
        from_lock: bool,
        /// Approve every requested capability without asking.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Install newer GitHub releases of installed plugins and scripts.
    Update {
        /// Only this plugin or script bundle.
        name: Option<String>,
        /// Approve new capabilities. Without it, a plugin that asks for
        /// more stays off until you approve.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Remove an installed plugin, or scripts `bricolo install` copied. A
    /// plugin's data folder stays.
    Uninstall { name: String },
    /// Check a built plugin folder without installing it (prints JSON), or
    /// compile a .lua script without running it.
    Check { path: PathBuf },
    /// Bundle a built plugin folder, or a folder of scripts, into one
    /// file to attach to a GitHub release.
    Pack {
        dir: PathBuf,
        /// Where to write the bundle.
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Print the standard library WIT: `bricolo wit` lists the interfaces,
    /// `bricolo wit <name>` prints one, `bricolo wit all` the whole package.
    Wit {
        name: Option<String>,
        /// Write the files to this folder instead of printing them.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Rebuild and reinstall a plugin folder each time its sources change.
    Dev { dir: PathBuf },
    /// Print the end of today's log file.
    Logs {
        /// Only lines from this plugin.
        #[arg(long)]
        plugin: Option<String>,
        #[arg(long, short = 'n', default_value_t = 50)]
        lines: usize,
    },
    /// Print this node's endpoint id and how a peer adds it.
    Id {
        /// The name to suggest for this node in the peer's peers.toml.
        #[arg(long)]
        name: Option<String>,
    },
    /// Manage peers.toml: the devices this node trusts.
    #[command(subcommand)]
    Peer(PeerCommand),
    /// Run the node at login: a LaunchAgent on macOS, a systemd user
    /// service on Linux. See docs/install.md.
    Service(service::ServiceArgs),
    /// Manage the sidecars in node.toml: local servers the node starts,
    /// health-checks and restarts.
    #[command(subcommand)]
    Sidecar(sidecar::SidecarCommand),
    /// Install the create-bricolo-app skill for coding agents (Claude Code,
    /// Codex, Gemini CLI, OpenCode, Cursor and others).
    #[command(subcommand)]
    Skill(skill::SkillCommand),
    /// Run one capability call through the running node's router, for
    /// debugging. Prints the result as JSON.
    Call {
        /// A routable capability: tts, speech, disk or clipboard.
        capability: String,
        method: String,
        /// The arguments as a JSON array, like '["~/notes/a.md"]'.
        #[arg(default_value = "[]")]
        args: String,
        /// A disk grant for this call, like disk:~/Notes. Repeatable.
        #[arg(long = "grant")]
        grants: Vec<String>,
    },
    /// Connect an AI agent to the mesh over MCP. With no subcommand, pipes
    /// standard input and output to the running node's gateway, for
    /// clients that start a command, like `claude mcp add bricolo -- bricolo mcp`.
    /// See docs/mcp.md.
    Mcp {
        #[command(subcommand)]
        command: Option<mcp::McpCommand>,
    },
}

#[derive(Subcommand)]
enum PeerCommand {
    /// Trust a device: append it to peers.toml.
    Add {
        /// The name rules.toml uses for the device, like "desktop".
        name: String,
        /// Its endpoint id, from `bricolo id` on that device.
        id: String,
        /// Capabilities it may call on this node, like tts,speech,disk:~/Shared.
        #[arg(long, value_delimiter = ',')]
        allow: Vec<String>,
        /// A direct address to try, like 192.168.1.20:7777. Repeatable.
        #[arg(long = "addr")]
        addrs: Vec<SocketAddr>,
    },
    /// List the peers and, if a node is running, their state.
    List,
    /// List the devices each mesh transport can see, such as the machines
    /// on the tailnet. Needs a running node.
    Discover,
    /// Make the running node reread peers.toml.
    Reload,
}

fn paths() -> Result<Paths> {
    Paths::from_env()
        .context("cannot find the home folder; set BRICOLO_CONFIG_DIR and BRICOLO_DATA_DIR")
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Replaces this process with a new run of the same program and
/// arguments, after `restart-node`. The pid stays, so launchd, systemd or
/// the hub that started the node keep watching it, and macOS answers the
/// grants afresh.
#[cfg(unix)]
fn restart_in_place() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().context("cannot find this program")?;
    let err = std::process::Command::new(&exe)
        .args(std::env::args_os().skip(1))
        .exec();
    Err(err).with_context(|| format!("cannot run {} again", exe.display()))
}

#[cfg(not(unix))]
fn restart_in_place() -> Result<()> {
    anyhow::bail!("restart-node works on macOS and Linux")
}

fn run(command: Command) -> Result<ExitCode> {
    let paths = paths()?;
    let runtime = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("cannot start Tokio")
    };
    match command {
        // The main thread runs the OS event loop (macOS); the node and
        // Tokio run on another thread.
        Command::Run => {
            let ended = bricolo_providers::desktop::run(move |desktop| {
                runtime()?.block_on(control::run_node(paths, desktop))
            })?;
            if ended == control::Ended::Restart {
                restart_in_place()?;
            }
        }
        Command::Status => return runtime()?.block_on(control::status(&paths)),
        Command::Fire { plugin, id } => {
            let request = json!({ "cmd": "fire", "plugin": plugin, "hotkey": id });
            return runtime()?.block_on(control::send(&paths, request));
        }
        Command::Permissions { local: true, .. } => {
            println!(
                "{}",
                serde_json::to_string(&bricolo_providers::permissions::check())?
            );
        }
        Command::Permissions { request, .. } => {
            return runtime()?.block_on(control::permissions(&paths, request));
        }
        Command::New { list: true, .. } => scaffold::print_templates(scaffold::PLUGIN_TEMPLATES),
        Command::New {
            name,
            template,
            dir,
            port,
            ..
        } => {
            let name = name.context("give the plugin a name")?;
            let created = scaffold::new_plugin(&name, &dir, &template, port)?;
            println!("created {} from the {template} template", created.display());
            println!("test:    cd {} && cargo test", created.display());
            println!("build:   cargo build --target wasm32-wasip2 --release");
            println!("check:   bricolo check .");
            println!("install: bricolo install . -y");
        }
        Command::NewScript { list: true, .. } => {
            scaffold::print_templates(scaffold::SCRIPT_TEMPLATES)
        }
        Command::NewScript {
            name,
            template,
            port,
            ..
        } => {
            let name = name.context("give the script a name")?;
            let created = scaffold::new_script(&name, &paths.scripts_dir(), &template, port)?;
            println!("created {} from the {template} template", created.display());
            println!("a running node starts it now and reloads it on each save");
        }
        Command::NewSidecar { list: true, .. } => {
            scaffold::print_templates(scaffold::SIDECAR_TEMPLATES)
        }
        Command::NewSidecar {
            name,
            template,
            port,
            dir,
            ..
        } => {
            let name = name.context("give the sidecar a name")?;
            let created = scaffold::new_sidecar(&name, &dir, &template, port)?;
            let shown: Vec<String> = created.add_args.iter().map(|a| shell_word(a)).collect();
            println!(
                "created {} from the {template} template",
                created.dir.display()
            );
            println!("test:     cd {} && {}", created.dir.display(), created.test);
            if let Some(build) = &created.build {
                println!("build:    {build}");
            }
            println!("register: bricolo sidecar add {name} {}", shown.join(" "));
            println!("check:    bricolo sidecar list   # \"ready\" once /health answers");
            println!(
                "call:     POST http://127.0.0.1:{port}/process, from a script or a sidecar-client plugin"
            );
        }
        Command::Install {
            from_lock: true,
            yes,
            ..
        } => return plugins::install_from_lock(&paths, yes),
        Command::Install { source, yes, .. } => {
            let source = source.context("name what to install")?;
            return plugins::install(&paths, &source, yes);
        }
        Command::Update { name, yes } => return plugins::update(&paths, name.as_deref(), yes),
        Command::Uninstall { name } => plugins::uninstall(&paths, &name)?,
        Command::Check { path } => plugins::check(&path)?,
        Command::Pack { dir, out } => plugins::pack(&dir, &out)?,
        Command::Wit { name, out } => scaffold::wit(name.as_deref(), out.as_deref())?,
        Command::Dev { dir } => runtime()?.block_on(dev::run(paths, dir))?,
        Command::Logs { plugin, lines } => logs::print(&paths, plugin.as_deref(), lines)?,
        Command::Id { name } => mesh::print_id(&paths, name)?,
        Command::Peer(PeerCommand::Add {
            name,
            id,
            allow,
            addrs,
        }) => {
            mesh::add_peer(&paths, &name, &id, &allow, &addrs)?;
            return runtime()?.block_on(reload_peers(&paths, true));
        }
        Command::Peer(PeerCommand::List) => {
            let status = runtime()?
                .block_on(control::request(&paths, &json!({ "cmd": "status" })))
                .ok();
            mesh::list_peers(&paths, status.as_ref().map(|s| &s["mesh"]))?;
        }
        Command::Peer(PeerCommand::Discover) => {
            return runtime()?.block_on(mesh::discover(&paths));
        }
        Command::Peer(PeerCommand::Reload) => {
            return runtime()?.block_on(reload_peers(&paths, false));
        }
        Command::Call {
            capability,
            method,
            args,
            grants,
        } => return runtime()?.block_on(call(&paths, capability, method, &args, grants)),
        Command::Mcp { command } => return mcp::main(&paths, command),
        Command::Service(args) => return service::main(&paths, args),
        Command::Sidecar(command) => return sidecar::main(&paths, command),
        Command::Skill(command) => return skill::main(command),
    }
    Ok(ExitCode::SUCCESS)
}

/// `word` quoted for a POSIX shell when it needs it.
fn shell_word(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+=:,@%".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Asks the running node to reread peers.toml. With `quiet`, a node that
/// is not running is fine: it reads the file when it starts.
async fn reload_peers(paths: &Paths, quiet: bool) -> Result<ExitCode> {
    match control::request(paths, &json!({ "cmd": "reload-peers" })).await {
        Ok(answer) if answer.get("error").is_none() => {
            println!("the running node reloaded peers.toml");
            Ok(ExitCode::SUCCESS)
        }
        Ok(answer) => {
            eprintln!("error: {}", answer["error"]);
            Ok(ExitCode::FAILURE)
        }
        Err(_) if quiet => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("no node answers on {}: {e:#}", paths.socket().display());
            Ok(ExitCode::from(NOT_RUNNING))
        }
    }
}

async fn call(
    paths: &Paths,
    capability: String,
    method: String,
    args: &str,
    grants: Vec<String>,
) -> Result<ExitCode> {
    let args: Value = serde_json::from_str(args).context("the arguments must be a JSON array")?;
    anyhow::ensure!(args.is_array(), "the arguments must be a JSON array");
    let request = json!({
        "cmd": "call",
        "capability": capability,
        "method": method,
        "args": args,
        "grants": grants,
    });
    let answer = match control::request(paths, &request).await {
        Ok(answer) => answer,
        Err(e) => {
            eprintln!("no node answers on {}: {e:#}", paths.socket().display());
            return Ok(ExitCode::from(NOT_RUNNING));
        }
    };
    println!("{}", serde_json::to_string_pretty(&answer)?);
    Ok(if answer.get("error").is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
