//! `bricolo sidecar add|list|remove|reload|restart|logs`: edits the
//! `[sidecars.<name>]` sections of node.toml and tells a running node.
//!
//! node.toml is a file you edit by hand, so the edits go through
//! `toml_edit`: comments, order and formatting outside the section stay as
//! they were. Each edit is checked as a whole `NodeConfig` before the file
//! is replaced, so `bricolo run` never meets a node.toml that `bricolo sidecar` broke.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use bricolo_providers::sidecar::SidecarConfig;
use bricolo_runtime::Paths;
use bricolo_runtime::manifest::valid_name;
use bricolo_runtime::node_config::NodeConfig;
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

use crate::NOT_RUNNING;

#[derive(Subcommand)]
pub enum SidecarCommand {
    /// Add or replace [sidecars.<name>] in node.toml; a running node
    /// starts it at once.
    Add(AddArgs),
    /// The sidecars in node.toml and, if a node runs, their state.
    List,
    /// Remove [sidecars.<name>] from node.toml; a running node stops it.
    Remove { name: String },
    /// Make the running node reread the sidecars in node.toml.
    Reload,
    /// Stop a running sidecar and start it again.
    Restart { name: String },
    /// Print the end of a sidecar's output: <data>/logs/sidecar-<name>.log.
    Logs {
        name: String,
        #[arg(long, short = 'n', default_value_t = 50)]
        lines: usize,
    },
}

#[derive(Args, Clone, Debug, Default)]
pub struct AddArgs {
    /// The name, like "ocr": lowercase letters, digits and dashes.
    pub name: String,
    /// The program, like python3 or ~/ocr/.venv/bin/python. A relative
    /// path with a slash becomes absolute.
    #[arg(long)]
    pub command: String,
    /// Arguments; repeat the flag, or put them after `--`.
    #[arg(long = "args", num_args = 1..)]
    pub args: Vec<String>,
    #[arg(last = true)]
    pub trailing: Vec<String>,
    /// The working folder; a relative path becomes absolute.
    #[arg(long)]
    pub cwd: Option<String>,
    /// The port it listens on, on 127.0.0.1.
    #[arg(long)]
    pub port: u16,
    /// Default http://127.0.0.1:<port>/health.
    #[arg(long)]
    pub health_url: Option<String>,
    /// How long it may take to become ready before the status shows an
    /// error. Default 120.
    #[arg(long)]
    pub startup_secs: Option<u64>,
    /// An environment variable, KEY=VALUE. Repeatable.
    #[arg(long = "env")]
    pub env: Vec<String>,
    /// Replace a sidecar of the same name.
    #[arg(long)]
    pub force: bool,
}

pub fn main(paths: &Paths, command: SidecarCommand) -> Result<ExitCode> {
    let file = paths.node_toml();
    match command {
        SidecarCommand::Add(args) => {
            let cwd = std::env::current_dir()?;
            let config = to_config(&args, &cwd)?;
            let text = read(&file)?;
            let edited = add(&text, &args.name, &config, args.force)?;
            write(&file, &edited)?;
            println!(
                "added [sidecars.{}] to {}: {} on port {}",
                args.name,
                file.display(),
                config.command,
                config.port
            );
            Ok(tell_node(paths))
        }
        SidecarCommand::Remove { name } => {
            let edited = remove(&read(&file)?, &name)?;
            write(&file, &edited)?;
            println!("removed [sidecars.{name}] from {}", file.display());
            Ok(tell_node(paths))
        }
        SidecarCommand::List => {
            list(paths)?;
            Ok(ExitCode::SUCCESS)
        }
        SidecarCommand::Reload => match node_request(paths, json!({ "cmd": "reload-sidecars" })) {
            Some(answer) => Ok(report(&answer)),
            None => Ok(not_running(paths)),
        },
        SidecarCommand::Restart { name } => {
            let request = json!({ "cmd": "restart-sidecar", "sidecar": name });
            match node_request(paths, request) {
                Some(answer) if answer.get("error").is_none() => {
                    println!("restarting {name}");
                    Ok(ExitCode::SUCCESS)
                }
                Some(answer) => {
                    eprintln!("error: {}", answer["error"]);
                    Ok(ExitCode::FAILURE)
                }
                None => Ok(not_running(paths)),
            }
        }
        SidecarCommand::Logs { name, lines } => {
            let log = paths.logs_dir().join(format!("sidecar-{name}.log"));
            let text = std::fs::read_to_string(&log)
                .with_context(|| format!("cannot read {}", log.display()))?;
            let all: Vec<&str> = text.lines().collect();
            for line in &all[all.len().saturating_sub(lines)..] {
                println!("{line}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The section `bricolo sidecar add` writes, from its flags. Relative paths
/// become absolute against `cwd`, because the node starts the sidecar
/// from another folder.
pub fn to_config(args: &AddArgs, cwd: &Path) -> Result<SidecarConfig> {
    if !valid_name(&args.name) {
        bail!(
            "name {:?} must be lowercase letters, digits and dashes",
            args.name
        );
    }
    if args.port == 0 {
        bail!("--port must be a port number from 1 to 65535");
    }
    let absolute = |s: &str| -> String {
        let path = Path::new(s);
        if path.is_relative() && !s.starts_with("~/") && s.contains('/') {
            let path = path.strip_prefix("./").unwrap_or(path);
            cwd.join(path).to_string_lossy().into_owned()
        } else {
            s.to_string()
        }
    };
    let mut env = BTreeMap::new();
    for pair in &args.env {
        let (key, val) = pair
            .split_once('=')
            .filter(|(k, _)| !k.is_empty())
            .with_context(|| format!("--env {pair:?} must be KEY=VALUE"))?;
        env.insert(key.to_string(), val.to_string());
    }
    let cwd_value = args.cwd.as_deref().map(|d| {
        if Path::new(d).is_relative() && !d.starts_with("~/") && d != "~" {
            cwd.join(d).to_string_lossy().into_owned()
        } else {
            d.to_string()
        }
    });
    Ok(SidecarConfig {
        command: absolute(&args.command),
        args: args.args.iter().chain(&args.trailing).cloned().collect(),
        cwd: cwd_value,
        env,
        port: args.port,
        health_url: args.health_url.clone(),
        startup_secs: args.startup_secs.unwrap_or(120),
    })
}

/// node.toml's text with `[sidecars.<name>]` set to `config`.
pub fn add(text: &str, name: &str, config: &SidecarConfig, force: bool) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("node.toml is not valid TOML")?;
    let current = parse(text)?;
    if current.sidecars.contains_key(name) && !force {
        bail!("[sidecars.{name}] already exists; add --force to replace it");
    }
    if let Some((other, _)) = current
        .sidecars
        .iter()
        .find(|(other, c)| *other != name && c.port == config.port)
    {
        bail!("port {} is already used by [sidecars.{other}]", config.port);
    }
    let sidecars = doc.entry("sidecars").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let sidecars = sidecars
        .as_table_like_mut()
        .context("`sidecars` in node.toml must be a table")?;
    let mut table = Table::new();
    table.insert("command", value(&config.command));
    if !config.args.is_empty() {
        table.insert("args", value(config.args.iter().collect::<Array>()));
    }
    if let Some(cwd) = &config.cwd {
        table.insert("cwd", value(cwd));
    }
    table.insert("port", value(i64::from(config.port)));
    if let Some(url) = &config.health_url {
        table.insert("health_url", value(url));
    }
    if config.startup_secs != 120 {
        table.insert("startup_secs", value(config.startup_secs as i64));
    }
    if !config.env.is_empty() {
        let mut env = InlineTable::new();
        for (k, v) in &config.env {
            env.insert(k, v.into());
        }
        table.insert("env", value(env));
    }
    sidecars.insert(name, Item::Table(table));
    let out = doc.to_string();
    parse(&out).context("node.toml would be invalid after the change")?;
    Ok(out)
}

/// node.toml's text without `[sidecars.<name>]`.
pub fn remove(text: &str, name: &str) -> Result<String> {
    let mut doc: DocumentMut = text.parse().context("node.toml is not valid TOML")?;
    let removed = doc
        .get_mut("sidecars")
        .and_then(Item::as_table_like_mut)
        .and_then(|t| t.remove(name));
    if removed.is_none() {
        bail!("there is no [sidecars.{name}] in node.toml");
    }
    let out = doc.to_string();
    parse(&out).with_context(|| format!("cannot remove [sidecars.{name}]"))?;
    Ok(out)
}

fn parse(text: &str) -> Result<NodeConfig> {
    let config: NodeConfig = toml::from_str(text)?;
    config.validate().map_err(anyhow::Error::msg)?;
    Ok(config)
}

pub(crate) fn read(file: &Path) -> Result<String> {
    match std::fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
    }
}

/// Through a temporary file and a rename, so a node rereading it never
/// sees half a file.
pub(crate) fn write(file: &Path, text: &str) -> Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp: PathBuf = dir.join(".node.toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, file).with_context(|| format!("cannot replace {}", file.display()))
}

fn list(paths: &Paths) -> Result<()> {
    let text = read(&paths.node_toml())?;
    let config = parse(&text).context("cannot read node.toml")?;
    let states: BTreeMap<String, Value> = node_request(paths, json!({ "cmd": "status" }))
        .and_then(|s| s["sidecars"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| Some((s["name"].as_str()?.to_string(), s)))
        .collect();
    if config.sidecars.is_empty() {
        println!("no sidecars in {}", paths.node_toml().display());
        return Ok(());
    }
    println!("{:<16} {:<6} {:<11} command", "name", "port", "state");
    for (name, c) in &config.sidecars {
        let state = match states.get(name) {
            Some(s) => s["state"].as_str().unwrap_or("?").to_string(),
            None if states.is_empty() => "-".into(),
            None => "not loaded".into(),
        };
        let mut command = c.command.clone();
        for a in &c.args {
            command.push(' ');
            command.push_str(a);
        }
        println!("{name:<16} {:<6} {state:<11} {command}", c.port);
        if let Some(error) = states.get(name).and_then(|s| s["error"].as_str()) {
            println!("{:<16} last error: {error}", "");
        }
    }
    if states.is_empty() {
        println!("(no node is running)");
    }
    Ok(())
}

fn node_request(paths: &Paths, request: Value) -> Option<Value> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime
        .block_on(crate::control::request(paths, &request))
        .ok()
}

/// After an edit: a running node applies it now; otherwise it reads the
/// file when it starts.
fn tell_node(paths: &Paths) -> ExitCode {
    match node_request(paths, json!({ "cmd": "reload-sidecars" })) {
        Some(answer) => report(&answer),
        None => {
            println!("no node is running; `bricolo run` starts the sidecars in node.toml");
            ExitCode::SUCCESS
        }
    }
}

fn report(answer: &Value) -> ExitCode {
    if let Some(error) = answer.get("error") {
        eprintln!("error: the running node refused node.toml: {error}");
        return ExitCode::FAILURE;
    }
    let s = &answer["sidecars"];
    let names = |key: &str| -> String {
        s[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let mut parts = Vec::new();
    for (key, verb) in [
        ("added", "started"),
        ("restarted", "restarted"),
        ("removed", "stopped"),
    ] {
        let list = names(key);
        if !list.is_empty() {
            parts.push(format!("{verb} {list}"));
        }
    }
    if parts.is_empty() {
        println!("the running node has nothing to change");
    } else {
        println!(
            "the running node {}; `bricolo sidecar list` shows when it is ready",
            parts.join("; ")
        );
    }
    ExitCode::SUCCESS
}

fn not_running(paths: &Paths) -> ExitCode {
    eprintln!("no node answers on {}", paths.socket().display());
    ExitCode::from(NOT_RUNNING)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE_TOML: &str = "\
# My node.
name = \"mac\"

[sidecars.stt]   # speech to text
command = \"~/stt/bin/python\"
args = [\"~/stt/server.py\"]
port = 8765

[speech]
sidecar = \"stt\"
";

    fn args(name: &str, command: &str, port: u16) -> AddArgs {
        AddArgs {
            name: name.into(),
            command: command.into(),
            port,
            ..AddArgs::default()
        }
    }

    #[test]
    fn add_keeps_comments_and_other_sections() {
        let mut a = args("ocr", "./ocr/run", 8801);
        a.args = vec!["--model".into(), "small".into()];
        a.trailing = vec!["-v".into()];
        a.cwd = Some("ocr".into());
        a.env = vec!["MODEL=small".into(), "A=b=c".into()];
        a.startup_secs = Some(300);
        let config = to_config(&a, Path::new("/work")).unwrap();
        // Joined like the code does, so the separators match on Windows.
        let work = Path::new("/work");
        assert_eq!(
            config.command,
            work.join("ocr/run").to_string_lossy().as_ref()
        );
        assert_eq!(config.args, ["--model", "small", "-v"]);
        assert_eq!(
            config.cwd.as_deref(),
            Some(work.join("ocr").to_string_lossy().as_ref())
        );
        assert_eq!(config.env["A"], "b=c");
        let out = add(NODE_TOML, "ocr", &config, false).unwrap();
        assert!(out.starts_with("# My node.\nname = \"mac\"\n"), "{out}");
        assert!(out.contains("[sidecars.stt]   # speech to text"), "{out}");
        // toml_edit picks the quoting (a literal string for `\` paths), so the
        // command is checked after parsing, below.
        assert!(out.contains("[sidecars.ocr]\ncommand = "), "{out}");
        assert!(out.contains("startup_secs = 300"), "{out}");
        assert!(!out.contains("\n[sidecars]\n"), "{out}");
        let parsed = parse(&out).unwrap();
        assert_eq!(parsed.sidecars["ocr"], config);
        assert_eq!(parsed.sidecars["stt"].port, 8765);
    }

    #[test]
    fn add_to_an_empty_file_and_replace_with_force() {
        let config = to_config(&args("echo", "python3", 8800), Path::new("/w")).unwrap();
        assert_eq!(config.command, "python3");
        let out = add("", "echo", &config, false).unwrap();
        assert_eq!(out, "[sidecars.echo]\ncommand = \"python3\"\nport = 8800\n");
        let err = add(&out, "echo", &config, false).unwrap_err();
        assert!(err.to_string().contains("--force"), "{err}");
        let moved = to_config(&args("echo", "python3", 8801), Path::new("/w")).unwrap();
        let out = add(&out, "echo", &moved, true).unwrap();
        assert_eq!(parse(&out).unwrap().sidecars["echo"].port, 8801);
    }

    #[test]
    fn add_refuses_bad_names_ports_and_env() {
        let cwd = Path::new("/w");
        assert!(to_config(&args("Bad_Name", "x", 1), cwd).is_err());
        assert!(to_config(&args("ok", "x", 0), cwd).is_err());
        let mut a = args("ok", "x", 1);
        a.env = vec!["NOEQUALS".into()];
        assert!(to_config(&a, cwd).is_err());
        let taken = to_config(&args("other", "x", 8765), cwd).unwrap();
        let err = add(NODE_TOML, "other", &taken, false).unwrap_err();
        assert!(err.to_string().contains("[sidecars.stt]"), "{err}");
        assert!(add("not = [toml", "x", &taken, false).is_err());
    }

    #[test]
    fn remove_keeps_the_rest_and_refuses_a_sidecar_in_use() {
        let config = to_config(&args("ocr", "python3", 8801), Path::new("/w")).unwrap();
        let with = add(NODE_TOML, "ocr", &config, false).unwrap();
        let without = remove(&with, "ocr").unwrap();
        assert!(!without.contains("ocr"), "{without}");
        assert!(without.contains("[sidecars.stt]   # speech to text"));
        assert!(remove(&without, "ocr").is_err());
        // [speech] names stt.
        let err = remove(NODE_TOML, "stt").unwrap_err();
        assert!(format!("{err:#}").contains("[speech]"), "{err:#}");
    }
}
