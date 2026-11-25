//! `bricolo skill install|path|snippet`: writes the create-bricolo-app skill, which
//! is compiled into `bricolo` (see build.rs), where coding agents look for
//! skills. Every agent below reads the open Agent Skills format
//! (agentskills.io): a folder holding SKILL.md.
//!
//! Where each agent looks, checked in November 2026
//! (skills/create-bricolo-app/references/agents.md has the sources):
//!
//! | Agent | User folder | Also reads |
//! |---|---|---|
//! | Claude Code | `~/.claude/skills` | |
//! | Codex CLI | `~/.agents/skills` | |
//! | Gemini CLI | `~/.gemini/skills` | `~/.agents/skills` |
//! | OpenCode | `~/.config/opencode/skills` | `~/.claude/skills`, `~/.agents/skills` |
//! | Cursor | `~/.cursor/skills` | `~/.agents/skills`, `~/.claude/skills`, `~/.codex/skills` |
//!
//! So `--agent all` writes `~/.claude/skills` and `~/.agents/skills`,
//! which together reach all five. Agents without a skills folder get
//! `bricolo skill snippet`, a paragraph for their AGENTS.md or GEMINI.md.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/skill.rs"));
}

pub const NAME: &str = "create-bricolo-app";
/// Written next to SKILL.md: the `bricolo` version that installed the copy.
const MARKER: &str = ".bricolo-version";

#[derive(Subcommand)]
pub enum SkillCommand {
    /// Write the create-bricolo-app skill where coding agents find it.
    Install(InstallArgs),
    /// Print where the skill is installed, one folder per line.
    Path,
    /// Print a paragraph for AGENTS.md or GEMINI.md that points an agent
    /// without a skills folder at the installed skill.
    Snippet,
}

#[derive(Args, Clone, Debug)]
pub struct InstallArgs {
    /// Which agents; comma-separated or repeated. `all` is claude and
    /// agents, which every listed agent reads.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "all")]
    pub agent: Vec<Agent>,
    /// Also install into this skills folder (the skill goes in
    /// <dir>/create-bricolo-app), like a project's .agents/skills.
    #[arg(long)]
    pub dir: Vec<PathBuf>,
    /// Link instead of copying: the first folder gets the files (or
    /// --source is used as is) and the others become symbolic links to it.
    #[arg(long)]
    pub link: bool,
    /// With --link: a skill folder to link to, like the repository's
    /// skills/create-bricolo-app while you edit it.
    #[arg(long, requires = "link")]
    pub source: Option<PathBuf>,
    /// Print what would be written, and write nothing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Agent {
    /// Claude Code: ~/.claude/skills.
    Claude,
    /// Codex CLI: ~/.agents/skills.
    Codex,
    /// Gemini CLI: ~/.gemini/skills.
    Gemini,
    /// OpenCode: ~/.config/opencode/skills.
    Opencode,
    /// Cursor: ~/.cursor/skills.
    Cursor,
    /// The shared folder ~/.agents/skills: Codex, Gemini, OpenCode, Cursor.
    Agents,
    /// claude and agents.
    All,
}

/// The folders the targets resolve against, so tests can use a temp home.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME`, for OpenCode.
    pub xdg_config: Option<PathBuf>,
    /// `$CLAUDE_CONFIG_DIR`, which moves Claude Code's ~/.claude.
    pub claude_config: Option<PathBuf>,
}

impl Env {
    pub fn from_process() -> Result<Self> {
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Ok(Self {
            home: bricolo_runtime::paths::home_dir().context("cannot find the home folder")?,
            xdg_config: var("XDG_CONFIG_HOME"),
            claude_config: var("CLAUDE_CONFIG_DIR"),
        })
    }

    /// The skills folder of one agent; the skill goes in `<it>/create-bricolo-app`.
    pub fn skills_dir(&self, agent: Agent) -> PathBuf {
        match agent {
            Agent::Claude => self
                .claude_config
                .clone()
                .unwrap_or_else(|| self.home.join(".claude"))
                .join("skills"),
            Agent::Codex | Agent::Agents | Agent::All => self.home.join(".agents").join("skills"),
            Agent::Gemini => self.home.join(".gemini").join("skills"),
            Agent::Opencode => self
                .xdg_config
                .clone()
                .unwrap_or_else(|| self.home.join(".config"))
                .join("opencode")
                .join("skills"),
            Agent::Cursor => self.home.join(".cursor").join("skills"),
        }
    }

    /// Where `bricolo skill path` looks, in this order.
    fn known(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = [
            Agent::Agents,
            Agent::Claude,
            Agent::Gemini,
            Agent::Opencode,
            Agent::Cursor,
        ]
        .into_iter()
        .map(|a| self.skills_dir(a).join(NAME))
        .collect();
        dirs.dedup();
        dirs
    }
}

/// The skill folders to write, without repeats, in a stable order.
pub fn targets(env: &Env, agents: &[Agent], extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut expanded = Vec::new();
    for agent in agents {
        match agent {
            Agent::All => expanded.extend([Agent::Agents, Agent::Claude]),
            other => expanded.push(*other),
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    let all = expanded
        .iter()
        .map(|a| env.skills_dir(*a))
        .chain(extra.iter().cloned());
    for dir in all {
        let path = dir.join(NAME);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

pub fn main(command: SkillCommand) -> Result<ExitCode> {
    let env = Env::from_process()?;
    match command {
        SkillCommand::Install(args) => {
            let cwd = std::env::current_dir()?;
            let extra: Vec<PathBuf> = args.dir.iter().map(|d| cwd.join(d)).collect();
            let targets = targets(&env, &args.agent, &extra);
            let source = args.source.as_ref().map(|s| cwd.join(s));
            let plan = plan(&targets, args.link, source.as_deref())?;
            if args.dry_run {
                for step in &plan {
                    println!("would {step}");
                }
                return Ok(ExitCode::SUCCESS);
            }
            for step in &plan {
                apply(step)?;
                println!("{step}");
            }
            println!("restart the agent, or start a new session, so it sees the skill");
            Ok(ExitCode::SUCCESS)
        }
        SkillCommand::Path => {
            let found = installed(&env);
            if found.is_empty() {
                eprintln!("the skill is not installed; run `bricolo skill install`");
                return Ok(ExitCode::FAILURE);
            }
            for (path, detail) in found {
                println!("{}{detail}", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        SkillCommand::Snippet => {
            let path = installed(&env)
                .into_iter()
                .next()
                .map(|(p, _)| p)
                .unwrap_or_else(|| env.skills_dir(Agent::Agents).join(NAME));
            print!("{}", snippet(&path));
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// One change to the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Write the embedded skill to this folder, replacing an older copy.
    Write(PathBuf),
    /// Make `link` a symbolic link to `to`, replacing an older copy.
    Link { link: PathBuf, to: PathBuf },
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::Write(dir) => write!(
                f,
                "write {} ({} files)",
                dir.display(),
                embedded::FILES.len()
            ),
            Step::Link { link, to } => write!(f, "link {} -> {}", link.display(), to.display()),
        }
    }
}

/// The steps for `targets`. Copies by default. With `link`, the first
/// target gets the files, or `source` is used as is, and the rest link to
/// it. Refuses a target that holds something other than this skill.
pub fn plan(targets: &[PathBuf], link: bool, source: Option<&Path>) -> Result<Vec<Step>> {
    for target in targets {
        check_replaceable(target)?;
    }
    if let Some(source) = source {
        if !source.join("SKILL.md").is_file() {
            bail!("{} has no SKILL.md", source.display());
        }
        let to = source
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", source.display()))?;
        return Ok(targets
            .iter()
            .map(|t| Step::Link {
                link: t.clone(),
                to: to.clone(),
            })
            .collect());
    }
    let Some((first, rest)) = targets.split_first() else {
        bail!("no target folders");
    };
    let mut steps = vec![Step::Write(first.clone())];
    for target in rest {
        steps.push(if link {
            Step::Link {
                link: target.clone(),
                to: first.clone(),
            }
        } else {
            Step::Write(target.clone())
        });
    }
    Ok(steps)
}

/// A target may be missing, a symbolic link, or a folder holding this
/// skill. Anything else is the user's and stays.
fn check_replaceable(target: &Path) -> Result<()> {
    let Ok(meta) = target.symlink_metadata() else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() && is_this_skill(target) {
        return Ok(());
    }
    bail!(
        "{} exists and is not the {NAME} skill; move it away first",
        target.display()
    )
}

fn is_this_skill(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("SKILL.md"))
        .map(|text| frontmatter_name(&text).as_deref() == Some(NAME))
        .unwrap_or(false)
}

/// The `name:` of a SKILL.md's YAML front matter.
pub fn frontmatter_name(text: &str) -> Option<String> {
    let rest = text.strip_prefix("---\n")?;
    let (front, _) = rest.split_once("\n---")?;
    front
        .lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().trim_matches('"').to_string())
}

/// Removes what [`check_replaceable`] allowed: a link or an older copy.
fn clear(target: &Path) -> Result<()> {
    match target.symlink_metadata() {
        Err(_) => Ok(()),
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(target)
            .with_context(|| format!("cannot remove the link {}", target.display())),
        Ok(_) => {
            check_replaceable(target)?;
            std::fs::remove_dir_all(target)
                .with_context(|| format!("cannot remove {}", target.display()))
        }
    }
}

pub fn apply(step: &Step) -> Result<()> {
    match step {
        Step::Write(target) => {
            let parent = target.parent().context("the target has no parent folder")?;
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
            // Into a fresh folder next to it, then swapped in, so an agent
            // never reads half a skill.
            let fresh = parent.join(format!(".{NAME}.new"));
            let _ = std::fs::remove_dir_all(&fresh);
            for (rel, text) in embedded::FILES {
                let path = fresh.join(rel);
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(&path, text)
                    .with_context(|| format!("cannot write {}", path.display()))?;
            }
            std::fs::write(
                fresh.join(MARKER),
                format!("{}\n", env!("CARGO_PKG_VERSION")),
            )?;
            clear(target)?;
            std::fs::rename(&fresh, target)
                .with_context(|| format!("cannot move the skill to {}", target.display()))
        }
        Step::Link { link, to } => {
            let parent = link.parent().context("the target has no parent folder")?;
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
            clear(link)?;
            symlink(to, link)
        }
    }
}

#[cfg(unix)]
fn symlink(to: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(to, link)
        .with_context(|| format!("cannot link {} to {}", link.display(), to.display()))
}

#[cfg(not(unix))]
fn symlink(_: &Path, _: &Path) -> Result<()> {
    bail!("--link needs a Unix system; install without it")
}

/// Each known folder that holds the skill, with where it links or which
/// `bricolo` wrote it.
pub fn installed(env: &Env) -> Vec<(PathBuf, String)> {
    env.known()
        .into_iter()
        .filter(|dir| dir.join("SKILL.md").is_file())
        .map(|dir| {
            let detail = match std::fs::read_link(&dir) {
                Ok(to) => format!(" -> {}", to.display()),
                Err(_) => std::fs::read_to_string(dir.join(MARKER))
                    .map(|v| format!(" (bricolo {})", v.trim()))
                    .unwrap_or_default(),
            };
            (dir, detail)
        })
        .collect()
}

/// A paragraph for an agent's AGENTS.md, GEMINI.md or CLAUDE.md.
pub fn snippet(path: &Path) -> String {
    format!(
        "## bricolo apps\n\n\
         When I ask for a bricolo app (a hotkey, clipboard, window, voice, timer \
         or HTTP automation, a Lua script, a WASM plugin or a sidecar), read \
         `{skill}/SKILL.md` first and follow it. It links to the details in \
         `{skill}/references/`. Run commands through `bricolo`, `cargo` and the shell.\n",
        skill = path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &Path) -> Env {
        Env {
            home: home.to_path_buf(),
            xdg_config: None,
            claude_config: None,
        }
    }

    #[test]
    fn all_means_the_shared_folder_and_claude() {
        let e = env(Path::new("/h"));
        let all = targets(&e, &[Agent::All], &[]);
        assert_eq!(
            all,
            [
                PathBuf::from("/h/.agents/skills/create-bricolo-app"),
                PathBuf::from("/h/.claude/skills/create-bricolo-app"),
            ]
        );
        let each = targets(
            &e,
            &[
                Agent::Codex,
                Agent::Agents,
                Agent::Gemini,
                Agent::Opencode,
                Agent::Cursor,
            ],
            &[PathBuf::from("/p/.agents/skills")],
        );
        assert_eq!(
            each,
            [
                "/h/.agents/skills/create-bricolo-app",
                "/h/.gemini/skills/create-bricolo-app",
                "/h/.config/opencode/skills/create-bricolo-app",
                "/h/.cursor/skills/create-bricolo-app",
                "/p/.agents/skills/create-bricolo-app",
            ]
            .map(PathBuf::from)
        );
        let moved = Env {
            xdg_config: Some("/x".into()),
            claude_config: Some("/c".into()),
            ..e
        };
        assert_eq!(
            moved.skills_dir(Agent::Opencode),
            Path::new("/x/opencode/skills")
        );
        assert_eq!(moved.skills_dir(Agent::Claude), Path::new("/c/skills"));
    }

    #[test]
    fn the_embedded_skill_is_complete() {
        let names: Vec<&str> = embedded::FILES.iter().map(|(p, _)| *p).collect();
        for needed in [
            "SKILL.md",
            "references/agents.md",
            "references/app-design.md",
            "references/capabilities.md",
            "references/sidecars.md",
            "references/troubleshooting.md",
        ] {
            assert!(names.contains(&needed), "{needed} is not embedded");
        }
        assert!(!names.iter().any(|n| n.starts_with("evals/")));
        let skill = embedded::FILES
            .iter()
            .find(|(p, _)| *p == "SKILL.md")
            .unwrap()
            .1;
        assert_eq!(frontmatter_name(skill).as_deref(), Some(NAME));
    }

    // --link makes symlinks, which `symlink` refuses off Unix.
    #[cfg(unix)]
    #[test]
    fn installs_copies_and_replaces_older_copies_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let e = env(dir.path());
        let all = targets(&e, &[Agent::All], &[]);
        for step in plan(&all, false, None).unwrap() {
            apply(&step).unwrap();
        }
        for target in &all {
            assert!(is_this_skill(target), "{}", target.display());
            assert!(target.join("references/sidecars.md").is_file());
        }
        let found = installed(&e);
        assert_eq!(found.len(), 2);
        assert!(found[0].1.contains("(bricolo "), "{found:?}");

        // Again with --link: claude's copy becomes a link to the shared one.
        for step in plan(&all, true, None).unwrap() {
            apply(&step).unwrap();
        }
        let claude = &all[1];
        assert_eq!(std::fs::read_link(claude).unwrap(), all[0]);
        assert!(claude.join("SKILL.md").is_file());
        // And back to copies: the link is replaced, not followed.
        for step in plan(&all, false, None).unwrap() {
            apply(&step).unwrap();
        }
        assert!(std::fs::read_link(claude).is_err());
        assert!(all[0].join("SKILL.md").is_file());
    }

    // --link makes symlinks, which `symlink` refuses off Unix.
    #[cfg(unix)]
    #[test]
    fn link_to_a_source_and_refuse_foreign_folders() {
        let dir = tempfile::tempdir().unwrap();
        let e = env(dir.path());
        let source = dir.path().join("checkout");
        std::fs::create_dir_all(&source).unwrap();
        assert!(plan(&targets(&e, &[Agent::Claude], &[]), true, Some(&source)).is_err());
        std::fs::write(
            source.join("SKILL.md"),
            "---\nname: create-bricolo-app\n---\n",
        )
        .unwrap();
        let claude = targets(&e, &[Agent::Claude], &[]);
        for step in plan(&claude, true, Some(&source)).unwrap() {
            apply(&step).unwrap();
        }
        assert_eq!(
            std::fs::read_link(&claude[0]).unwrap(),
            source.canonicalize().unwrap()
        );

        let foreign = targets(&e, &[Agent::Cursor], &[]);
        std::fs::create_dir_all(&foreign[0]).unwrap();
        std::fs::write(foreign[0].join("SKILL.md"), "---\nname: other\n---\n").unwrap();
        let err = plan(&foreign, false, None).unwrap_err();
        assert!(
            err.to_string().contains("not the create-bricolo-app skill"),
            "{err}"
        );
        assert!(foreign[0].join("SKILL.md").is_file());
    }

    #[test]
    fn snippet_names_the_skill_path() {
        let text = snippet(Path::new("/h/.agents/skills/create-bricolo-app"));
        assert!(text.contains("/h/.agents/skills/create-bricolo-app/SKILL.md"));
        assert!(text.starts_with("## bricolo apps\n"));
    }
}
