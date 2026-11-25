//! `bricolo service`: runs `bricolo run` at login, as a LaunchAgent on macOS, a
//! systemd user service on Linux and a logon task of Task Scheduler on
//! Windows. See docs/install.md.
//!
//! Each action is a plan of steps (write a file, run `launchctl`,
//! `systemctl` or `schtasks`) built by pure functions, so the tests check
//! the files and commands without touching launchd, systemd or Task
//! Scheduler. `--dry-run` prints the plan instead of carrying it out.
//!
//! On Windows the task runs `conhost.exe --headless cmd.exe /d /s /c
//! ""<data>\service\bricolo-service.cmd""`. The wrapper sets `BRICOLO_CONFIG_DIR`
//! and `BRICOLO_DATA_DIR` (a task has no environment of its own), sends the
//! output to the log, and restarts the node after a crash. conhost
//! `--headless` gives the console program a console without a window. The
//! task is registered from an XML file rather than `schtasks /SC ONLOGON
//! /TR`, which stops tasks after 72 hours, does not start them on battery,
//! and limits the command to 261 characters.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use bricolo_runtime::Paths;
use bricolo_runtime::paths::{CONFIG_ENV, DATA_ENV};
use clap::{Args, Subcommand, ValueEnum};

/// The LaunchAgent label, and the bundle id in crates/ui/tauri.conf.json.
pub const LABEL: &str = "com.pierre-mike.bricolo";
/// The LaunchAgent that opens `bricolo-ui` at login (`--ui`).
pub const UI_LABEL: &str = "com.pierre-mike.bricolo.ui";
/// The systemd user unit.
pub const UNIT: &str = "bricolo.service";
/// The Task Scheduler tasks for the node and for `bricolo-ui` (`--ui`).
pub const TASK: &str = "bricolo";
pub const UI_TASK: &str = "bricolo-ui";
/// The desktop entry for `bricolo-ui`, in applications/ and autostart/.
pub const DESKTOP_FILE: &str = "bricolo.desktop";
pub const APP_NAME: &str = "bricolo.app";

/// launchd starts agents with only the system folders on `PATH`. Sidecars
/// and `shell` grants often name programs from Homebrew or `~/.local/bin`.
const LAUNCHD_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// The session variables a desktop node needs, copied from the systemd
/// user manager's environment.
const PASS_ENVIRONMENT: &str = "DISPLAY WAYLAND_DISPLAY XAUTHORITY XDG_SESSION_TYPE XDG_CURRENT_DESKTOP DBUS_SESSION_BUS_ADDRESS";

#[derive(Args)]
pub struct ServiceArgs {
    #[command(subcommand)]
    action: Action,
    /// Print the files and commands instead of writing and running them.
    #[arg(long, global = true)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum Action {
    /// Start `bricolo run` at login and now, and restart it if it crashes.
    Install(InstallArgs),
    /// Stop the service and remove what `install` wrote.
    Uninstall,
    /// Show whether the service is loaded and running.
    Status,
    /// Start the installed service.
    Start,
    /// Stop the service until the next login or `bricolo service start`.
    Stop,
}

#[derive(Args)]
struct InstallArgs {
    /// The installed app (macOS). Default: /Applications/bricolo.app,
    /// else ~/Applications/bricolo.app.
    #[arg(long, conflicts_with = "binary")]
    app: Option<PathBuf>,
    /// A plain `bricolo` binary to run instead of the app's. Default on Linux:
    /// this `bricolo`.
    #[arg(long)]
    binary: Option<PathBuf>,
    /// Also open `bricolo-ui` (the app's, or the one next to `bricolo`) at login. On Linux this also
    /// adds it to the applications menu.
    #[arg(long)]
    ui: bool,
    /// Linux: start with the graphical session (desktop) or at login
    /// (headless). Default: desktop when DISPLAY or WAYLAND_DISPLAY is set.
    #[arg(long)]
    session: Option<Session>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Session {
    Desktop,
    Headless,
}

/// The service manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Init {
    /// launchd, in the `gui/<uid>` domain. Elsewhere only the tests build
    /// launchd plans.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Launchd { uid: u32 },
    /// systemd's user manager.
    Systemd,
    /// Task Scheduler, with a logon task for this user. Elsewhere only the
    /// tests build its plans.
    #[cfg_attr(not(windows), allow(dead_code))]
    TaskScheduler,
}

impl Init {
    fn current() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: getuid has no preconditions and cannot fail.
            Ok(Init::Launchd {
                uid: unsafe { libc::getuid() },
            })
        }
        #[cfg(target_os = "linux")]
        {
            Ok(Init::Systemd)
        }
        #[cfg(windows)]
        {
            Ok(Init::TaskScheduler)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            bail!("bricolo service supports macOS, Linux and Windows")
        }
    }
}

/// Where things go, from the environment.
pub struct Context {
    pub home: PathBuf,
    pub paths: Paths,
    /// `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR`, for the ones that are set. The
    /// service gets them too, so it uses the same folders.
    pub env: Vec<(String, String)>,
    /// `$XDG_CONFIG_HOME`, default `~/.config`.
    pub xdg_config: PathBuf,
    /// `$XDG_DATA_HOME`, default `~/.local/share`.
    pub xdg_data: PathBuf,
    /// Windows: `%USERDOMAIN%\%USERNAME%`, whose logon starts the task.
    pub user: String,
}

impl Context {
    fn from_env(paths: &Paths) -> Result<Self> {
        let home = bricolo_runtime::paths::home_dir().context("cannot find the home folder")?;
        let var = |name: &str| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let mut env = Vec::new();
        for name in [CONFIG_ENV, DATA_ENV] {
            if let Some(v) = var(name) {
                env.push((name.to_string(), absolute(&v)?.display().to_string()));
            }
        }
        let text = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let user = match (text("USERDOMAIN"), text("USERNAME")) {
            (Some(domain), Some(name)) => format!("{domain}\\{name}"),
            (None, Some(name)) => name,
            _ => String::new(),
        };
        Ok(Self {
            user,
            xdg_config: var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            xdg_data: var("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")),
            paths: Paths::new(absolute(&paths.config)?, absolute(&paths.data)?),
            home,
            env,
        })
    }

    pub fn launch_agent(&self, label: &str) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    }

    pub fn unit_file(&self) -> PathBuf {
        self.xdg_config.join("systemd/user").join(UNIT)
    }

    pub fn desktop_entry(&self) -> PathBuf {
        self.xdg_data.join("applications").join(DESKTOP_FILE)
    }

    pub fn autostart_entry(&self) -> PathBuf {
        self.xdg_config.join("autostart").join(DESKTOP_FILE)
    }

    /// Windows: the wrappers and task files `install` writes.
    pub fn task_dir(&self) -> PathBuf {
        self.paths.data.join("service")
    }

    /// The wrapper the task runs: `bricolo-service.cmd`, or `bricolo-ui.cmd`.
    pub fn task_wrapper(&self, task: &str) -> PathBuf {
        let name = if task == UI_TASK {
            "bricolo-ui"
        } else {
            "bricolo-service"
        };
        self.task_dir().join(format!("{name}.cmd"))
    }

    /// The task's definition, for `schtasks /Create /XML`.
    pub fn task_xml(&self, task: &str) -> PathBuf {
        self.task_dir().join(format!("{task}.xml"))
    }

    /// Where launchd (or the Windows wrapper) writes the node's stdout and stderr.
    pub fn service_log(&self, name: &str) -> PathBuf {
        self.paths.logs_dir().join(format!("{name}.log"))
    }
}

/// What to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub bricolo: PathBuf,
    /// `bricolo-ui`, to open at login.
    pub ui: Option<PathBuf>,
    /// Set when `bricolo` is inside the app bundle.
    pub app: Option<PathBuf>,
    pub session: Session,
}

/// One thing a plan does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    MakeDir(PathBuf),
    Write {
        path: PathBuf,
        text: String,
    },
    /// The same, as UTF-16 with a byte order mark: what `schtasks /XML`
    /// reads.
    WriteUtf16 {
        path: PathBuf,
        text: String,
    },
    /// Remove a file if it is there.
    Remove(PathBuf),
    Run {
        argv: Vec<String>,
        on_fail: OnFail,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnFail {
    Stop,
    Ignore,
    /// Try again up to this many times, half a second apart, then stop.
    Retry(u8),
}

fn run(argv: &[&str], on_fail: OnFail) -> Step {
    Step::Run {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        on_fail,
    }
}

fn path_str(p: &Path) -> &str {
    p.to_str().unwrap_or_default()
}

// ---- The files ------------------------------------------------------

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn plist_string(s: &str) -> String {
    format!("<string>{}</string>", xml_escape(s))
}

fn plist_env(cx: &Context) -> String {
    let path = format!("{}:{LAUNCHD_PATH}", cx.home.join(".local/bin").display());
    let mut vars = vec![("PATH".to_string(), path)];
    vars.extend(cx.env.iter().cloned());
    vars.iter()
        .map(|(k, v)| {
            format!(
                "\t\t<key>{}</key>\n\t\t{}\n",
                xml_escape(k),
                plist_string(v)
            )
        })
        .collect()
}

fn plist(
    label: &str,
    args: &[&str],
    log: &Path,
    keep_alive: bool,
    cx: &Context,
    app: bool,
) -> String {
    let args: String = args
        .iter()
        .map(|a| format!("\t\t{}\n", plist_string(a)))
        .collect();
    let log = plist_string(path_str(log));
    let keep_alive = if keep_alive {
        // Restart after a crash, not after a clean exit (SIGTERM, `stop`).
        "\t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n"
    } else {
        ""
    };
    // Shows the app's name and icon in System Settings > Login Items.
    let associated = if app {
        format!(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t{}\n\t</array>\n",
            plist_string(LABEL)
        )
    } else {
        String::new()
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `bricolo service install`. Remove with `bricolo service uninstall`. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	{label}
	<key>ProgramArguments</key>
	<array>
{args}	</array>
{associated}	<key>RunAtLoad</key>
	<true/>
{keep_alive}	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>EnvironmentVariables</key>
	<dict>
{env}	</dict>
	<key>StandardOutPath</key>
	{log}
	<key>StandardErrorPath</key>
	{log}
</dict>
</plist>
"#,
        label = plist_string(label),
        env = plist_env(cx),
    )
}

/// The LaunchAgent for the node.
pub fn launchd_plist(cx: &Context, target: &Target) -> String {
    let bricolo = path_str(&target.bricolo);
    plist(
        LABEL,
        &[bricolo, "run"],
        &cx.service_log("bricolo-service"),
        true,
        cx,
        target.app.is_some(),
    )
}

/// The LaunchAgent that opens `bricolo-ui` with only its menu bar item.
pub fn launchd_ui_plist(cx: &Context, target: &Target, ui: &Path) -> String {
    plist(
        UI_LABEL,
        &[path_str(ui), "--background"],
        &cx.service_log("bricolo-ui"),
        false,
        cx,
        target.app.is_some(),
    )
}

/// Quotes one word of an `ExecStart=` or `Environment=` line.
fn systemd_quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    if escaped.contains(char::is_whitespace) || escaped != s {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

/// The systemd user unit for the node.
pub fn systemd_unit(cx: &Context, target: &Target) -> String {
    let (after, wanted_by) = match target.session {
        // Starts once the desktop has put DISPLAY and the rest into the
        // user manager's environment, and stops with the session.
        Session::Desktop => (
            "After=graphical-session.target\nPartOf=graphical-session.target\n",
            "graphical-session.target",
        ),
        Session::Headless => ("", "default.target"),
    };
    let env: String = cx
        .env
        .iter()
        .map(|(k, v)| format!("Environment={}\n", systemd_quote(&format!("{k}={v}"))))
        .collect();
    format!(
        "# Written by `bricolo service install`. Remove with `bricolo service uninstall`.\n\
         [Unit]\n\
         Description=bricolo node\n\
         {after}\
         \n\
         [Service]\n\
         ExecStart={bricolo} run\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         PassEnvironment={PASS_ENVIRONMENT}\n\
         {env}\
         \n\
         [Install]\n\
         WantedBy={wanted_by}\n",
        bricolo = systemd_quote(path_str(&target.bricolo)),
    )
}

/// Quotes a path for a desktop entry's `Exec=` key.
fn desktop_quote(s: &str) -> String {
    let reserved = |c: char| " \t\n\"'\\><~|&;$*?#()`".contains(c);
    let escaped = s.replace('%', "%%");
    if !escaped.contains(reserved) {
        return escaped;
    }
    let mut out = String::from("\"");
    for c in escaped.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The desktop entry for `bricolo-ui`: in applications/ it opens the hub, in
/// autostart/ (`background`) only the tray.
pub fn desktop_entry(ui: &Path, background: bool) -> String {
    let mut exec = desktop_quote(path_str(ui));
    if background {
        exec.push_str(" --background");
    }
    format!(
        "# Written by `bricolo service install --ui`. Remove with `bricolo service uninstall`.\n\
         [Desktop Entry]\n\
         Type=Application\n\
         Name=bricolo\n\
         Comment=The hub for the bricolo node\n\
         Exec={exec}\n\
         Terminal=false\n\
         Categories=Utility;\n"
    )
}

/// Quotes a path or value for a `.cmd` file: `%` doubles, the rest is safe
/// inside double quotes (Windows paths cannot hold `"`).
fn cmd_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('%', "%%"))
}

/// The lines of a `.cmd` file, with the CRLF endings cmd expects.
fn cmd_file(lines: &[String]) -> String {
    lines.iter().map(|l| format!("{l}\r\n")).collect()
}

/// The start of both wrappers: UTF-8 for what follows (cmd reads a batch
/// file line by line in the console's code page), and the environment.
fn cmd_head(cx: &Context, what: &str) -> Vec<String> {
    let mut lines = vec![
        "@echo off".to_string(),
        format!(
            "rem {what} Written by `bricolo service install`. Remove with `bricolo service uninstall`."
        ),
        "chcp 65001 >nul".to_string(),
    ];
    for (k, v) in &cx.env {
        lines.push(format!("set {}", cmd_quote(&format!("{k}={v}"))));
    }
    lines
}

/// The node's wrapper: runs `bricolo run` with its output in the log, and again
/// 5 s after a crash, like systemd's `Restart=on-failure`; a clean exit
/// (Ctrl+Break, the console closing, logoff) ends it.
pub fn task_wrapper(cx: &Context, target: &Target) -> String {
    let mut lines = cmd_head(cx, "Runs the bricolo node for Task Scheduler.");
    lines.extend([
        ":run".to_string(),
        format!(
            "{} run >> {} 2>&1",
            cmd_quote(path_str(&target.bricolo)),
            cmd_quote(path_str(&cx.service_log("bricolo-service")))
        ),
        "if errorlevel 1 (".to_string(),
        "  ping -n 6 127.0.0.1 >nul".to_string(),
        "  goto run".to_string(),
        ")".to_string(),
    ]);
    cmd_file(&lines)
}

/// The hub's wrapper: opens `bricolo-ui` with only its tray icon.
pub fn task_ui_wrapper(cx: &Context, ui: &Path) -> String {
    let mut lines = cmd_head(cx, "Opens the bricolo hub for Task Scheduler.");
    lines.push(format!(
        "{} --background >> {} 2>&1",
        cmd_quote(path_str(ui)),
        cmd_quote(path_str(&cx.service_log("bricolo-ui")))
    ));
    cmd_file(&lines)
}

/// The task: at this user's logon, run the wrapper in a console without a
/// window, as the user, unelevated, with no time limit, on battery too.
pub fn task_definition(cx: &Context, description: &str, wrapper: &Path) -> String {
    let user = xml_escape(&cx.user);
    // /s: cmd strips the outer quotes only, so the path may hold spaces,
    // parentheses or `&`.
    let arguments = xml_escape(&format!(
        "--headless cmd.exe /d /s /c \"\"{}\"\"",
        path_str(wrapper)
    ));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<!-- Written by `bricolo service install`. Remove with `bricolo service uninstall`. -->
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>{description}</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>conhost.exe</Command>
      <Arguments>{arguments}</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        description = xml_escape(description),
    )
}

fn schtasks(args: &[&str], on_fail: OnFail) -> Step {
    let mut argv = vec!["schtasks"];
    argv.extend(args);
    run(&argv, on_fail)
}

// ---- The plans ------------------------------------------------------

fn domain(uid: u32) -> String {
    format!("gui/{uid}")
}

fn service(uid: u32, label: &str) -> String {
    format!("gui/{uid}/{label}")
}

pub fn install_plan(init: Init, cx: &Context, target: &Target) -> Vec<Step> {
    let mut steps = vec![Step::MakeDir(cx.paths.logs_dir())];
    match init {
        Init::Launchd { uid } => {
            let domain = domain(uid);
            let mut agents = vec![(LABEL, launchd_plist(cx, target))];
            if let Some(ui) = &target.ui {
                agents.push((UI_LABEL, launchd_ui_plist(cx, target, ui)));
            }
            for (label, text) in agents {
                let plist = cx.launch_agent(label);
                let plist = path_str(&plist).to_string();
                let service = service(uid, label);
                steps.extend([
                    // Unload an earlier install, so the new file takes effect.
                    run(&["launchctl", "bootout", &service], OnFail::Ignore),
                    Step::MakeDir(cx.launch_agent(label).parent().unwrap().into()),
                    Step::Write {
                        path: plist.clone().into(),
                        text,
                    },
                    // A service someone disabled refuses to bootstrap.
                    run(&["launchctl", "enable", &service], OnFail::Ignore),
                    // RunAtLoad starts it. Right after a bootout, launchd can
                    // still be tearing the old one down.
                    run(
                        &["launchctl", "bootstrap", &domain, &plist],
                        OnFail::Retry(10),
                    ),
                ]);
            }
        }
        Init::Systemd => {
            steps.extend([
                Step::MakeDir(cx.unit_file().parent().unwrap().into()),
                Step::Write {
                    path: cx.unit_file(),
                    text: systemd_unit(cx, target),
                },
            ]);
            if let Some(ui) = &target.ui {
                for (path, background) in
                    [(cx.desktop_entry(), false), (cx.autostart_entry(), true)]
                {
                    steps.push(Step::MakeDir(path.parent().unwrap().into()));
                    steps.push(Step::Write {
                        path,
                        text: desktop_entry(ui, background),
                    });
                }
            }
            steps.extend([
                run(&["systemctl", "--user", "daemon-reload"], OnFail::Stop),
                // An earlier install may have used the other target.
                run(&["systemctl", "--user", "disable", UNIT], OnFail::Ignore),
                run(&["systemctl", "--user", "enable", UNIT], OnFail::Stop),
                run(&["systemctl", "--user", "restart", UNIT], OnFail::Stop),
            ]);
        }
        Init::TaskScheduler => {
            let mut tasks = vec![(
                TASK,
                "The bricolo node (bricolo run).",
                task_wrapper(cx, target),
            )];
            if let Some(ui) = &target.ui {
                tasks.push((
                    UI_TASK,
                    "The bricolo hub (bricolo-ui), in the tray.",
                    task_ui_wrapper(cx, ui),
                ));
            }
            steps.push(Step::MakeDir(cx.task_dir()));
            for (task, description, wrapper) in tasks {
                let xml = cx.task_xml(task);
                steps.extend([
                    // Stops an earlier install's node, so the new wrapper runs.
                    schtasks(&["/End", "/TN", task], OnFail::Ignore),
                    Step::Write {
                        path: cx.task_wrapper(task),
                        text: wrapper,
                    },
                    Step::WriteUtf16 {
                        path: xml.clone(),
                        text: task_definition(cx, description, &cx.task_wrapper(task)),
                    },
                    // /F replaces an earlier install.
                    schtasks(
                        &["/Create", "/TN", task, "/XML", path_str(&xml), "/F"],
                        OnFail::Stop,
                    ),
                ]);
            }
            // Now, not only at the next logon.
            steps.push(schtasks(&["/Run", "/TN", TASK], OnFail::Stop));
        }
    }
    steps
}

pub fn uninstall_plan(init: Init, cx: &Context) -> Vec<Step> {
    match init {
        Init::Launchd { uid } => [UI_LABEL, LABEL]
            .iter()
            .flat_map(|label| {
                [
                    run(
                        &["launchctl", "bootout", &service(uid, label)],
                        OnFail::Ignore,
                    ),
                    Step::Remove(cx.launch_agent(label)),
                ]
            })
            .collect(),
        Init::Systemd => vec![
            run(
                &["systemctl", "--user", "disable", "--now", UNIT],
                OnFail::Ignore,
            ),
            Step::Remove(cx.unit_file()),
            Step::Remove(cx.desktop_entry()),
            Step::Remove(cx.autostart_entry()),
            run(&["systemctl", "--user", "daemon-reload"], OnFail::Ignore),
        ],
        Init::TaskScheduler => [UI_TASK, TASK]
            .iter()
            .flat_map(|task| {
                [
                    schtasks(&["/End", "/TN", task], OnFail::Ignore),
                    schtasks(&["/Delete", "/TN", task, "/F"], OnFail::Ignore),
                    Step::Remove(cx.task_wrapper(task)),
                    Step::Remove(cx.task_xml(task)),
                ]
            })
            .collect(),
    }
}

pub fn start_plan(init: Init, cx: &Context) -> Vec<Step> {
    match init {
        Init::Launchd { uid } => {
            let plist = cx.launch_agent(LABEL);
            vec![
                // Loaded already, unless `stop` unloaded it.
                run(
                    &["launchctl", "bootstrap", &domain(uid), path_str(&plist)],
                    OnFail::Ignore,
                ),
                run(
                    &["launchctl", "kickstart", &service(uid, LABEL)],
                    OnFail::Stop,
                ),
            ]
        }
        Init::Systemd => vec![run(&["systemctl", "--user", "start", UNIT], OnFail::Stop)],
        Init::TaskScheduler => vec![schtasks(&["/Run", "/TN", TASK], OnFail::Stop)],
    }
}

pub fn stop_plan(init: Init) -> Vec<Step> {
    match init {
        // Unloading keeps launchd from starting it again; the file stays,
        // so it starts at the next login.
        Init::Launchd { uid } => {
            vec![run(
                &["launchctl", "bootout", &service(uid, LABEL)],
                OnFail::Stop,
            )]
        }
        Init::Systemd => vec![run(&["systemctl", "--user", "stop", UNIT], OnFail::Stop)],
        // Ends the task's processes; the task stays, so it starts at the
        // next logon.
        Init::TaskScheduler => vec![schtasks(&["/End", "/TN", TASK], OnFail::Stop)],
    }
}

// ---- Carrying out a plan ----------------------------------------------

/// What a command did.
pub struct Ran {
    pub ok: bool,
    pub output: String,
}

/// Runs `launchctl`, `systemctl` and `schtasks`. The tests record the
/// calls instead.
pub trait Runner {
    fn run(&mut self, argv: &[String]) -> std::io::Result<Ran>;

    /// The wait before a retry.
    fn pause(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

pub struct System;

impl Runner for System {
    fn run(&mut self, argv: &[String]) -> std::io::Result<Ran> {
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()?;
        let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
        output.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(Ran {
            ok: out.status.success(),
            output,
        })
    }
}

/// Prints each step, and with `dry_run` only prints it.
pub fn apply(
    steps: &[Step],
    runner: &mut dyn Runner,
    out: &mut dyn Write,
    dry_run: bool,
) -> Result<()> {
    let verb = |done: &str, would: &str| {
        if dry_run {
            would.to_string()
        } else {
            done.to_string()
        }
    };
    for step in steps {
        match step {
            Step::MakeDir(dir) => {
                if !dry_run {
                    std::fs::create_dir_all(dir)
                        .with_context(|| format!("cannot create {}", dir.display()))?;
                }
            }
            Step::Write { path, text } | Step::WriteUtf16 { path, text } => {
                writeln!(out, "{} {}", verb("wrote", "would write"), path.display())?;
                if dry_run {
                    for line in text.lines() {
                        writeln!(out, "    {line}")?;
                    }
                } else {
                    let bytes = match step {
                        Step::WriteUtf16 { .. } => utf16_with_bom(text),
                        _ => text.as_bytes().to_vec(),
                    };
                    std::fs::write(path, bytes)
                        .with_context(|| format!("cannot write {}", path.display()))?;
                }
            }
            Step::Remove(path) => {
                if dry_run {
                    if path.exists() {
                        writeln!(out, "would remove {}", path.display())?;
                    }
                } else if path.exists() {
                    std::fs::remove_file(path)
                        .with_context(|| format!("cannot remove {}", path.display()))?;
                    writeln!(out, "removed {}", path.display())?;
                }
            }
            Step::Run { argv, on_fail } => {
                let line = argv.join(" ");
                if dry_run {
                    writeln!(out, "would run: {line}")?;
                    continue;
                }
                writeln!(out, "run: {line}")?;
                let tries = match on_fail {
                    OnFail::Retry(n) => 1 + *n as usize,
                    _ => 1,
                };
                let mut last = None;
                for attempt in 0..tries {
                    if attempt > 0 {
                        runner.pause();
                    }
                    let ran = runner
                        .run(argv)
                        .with_context(|| format!("cannot run {}", argv[0]))?;
                    if ran.ok {
                        last = None;
                        break;
                    }
                    last = Some(ran.output);
                }
                if let Some(output) = last {
                    let output = output.trim();
                    if *on_fail == OnFail::Ignore {
                        if !output.is_empty() {
                            writeln!(out, "    (ignored) {output}")?;
                        }
                    } else {
                        bail!("{line} failed: {output}");
                    }
                }
            }
        }
    }
    Ok(())
}

/// UTF-16LE with its byte order mark.
fn utf16_with_bom(text: &str) -> Vec<u8> {
    std::iter::once(0xfeff)
        .chain(text.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect()
}

// ---- The command --------------------------------------------------------

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn binary_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// `bricolo` inside bricolo.app. It is the main executable of a helper
/// app with its own bundle id: run from Contents/MacOS, LaunchServices took
/// the node for the app, and opening the app only brought the node forward.
pub const NODE_IN_APP: &str = "Contents/Helpers/bricolo.app/Contents/MacOS/bricolo";

fn app_node(app: &Path) -> PathBuf {
    app.join(NODE_IN_APP)
}

/// `bricolo-ui`, the app's main executable.
fn app_ui(app: &Path) -> PathBuf {
    app.join("Contents/MacOS/bricolo-ui")
}

/// The first installed app: /Applications, then ~/Applications.
pub fn find_app(home: &Path) -> Option<PathBuf> {
    [
        Path::new("/Applications").join(APP_NAME),
        home.join("Applications").join(APP_NAME),
    ]
    .into_iter()
    .find(|app| app_node(app).is_file())
}

fn resolve_target(init: Init, cx: &Context, args: &InstallArgs) -> Result<Target> {
    let (bricolo, app) = if let Some(binary) = &args.binary {
        (absolute(binary)?, None)
    } else if let Some(app) = &args.app {
        let app = absolute(app)?;
        (app_node(&app), Some(app))
    } else if let Init::Launchd { .. } = init {
        let app = find_app(&cx.home).with_context(|| {
            format!(
                "no {APP_NAME} in /Applications or ~/Applications; \
                 pass --app <path> or --binary <path>"
            )
        })?;
        (app_node(&app), Some(app))
    } else {
        let exe = std::env::current_exe().context("cannot find this bricolo")?;
        (exe.canonicalize().unwrap_or(exe), None)
    };
    anyhow::ensure!(bricolo.is_file(), "{} is not there", bricolo.display());
    let ui = if args.ui {
        let ui = match &app {
            Some(app) => app_ui(app),
            None => bricolo.with_file_name(binary_name("bricolo-ui")),
        };
        anyhow::ensure!(
            ui.is_file(),
            "--ui: no {}; build it with `just ui` or install the bundle",
            ui.display()
        );
        Some(ui)
    } else {
        None
    };
    let display = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|v| !v.is_empty()));
    let session = args.session.unwrap_or(if display {
        Session::Desktop
    } else {
        Session::Headless
    });
    Ok(Target {
        bricolo,
        ui,
        app,
        session,
    })
}

fn installed(init: Init, cx: &Context) -> PathBuf {
    match init {
        Init::Launchd { .. } => cx.launch_agent(LABEL),
        Init::Systemd => cx.unit_file(),
        Init::TaskScheduler => cx.task_wrapper(TASK),
    }
}

pub fn main(paths: &Paths, args: ServiceArgs) -> Result<ExitCode> {
    let init = Init::current()?;
    let cx = Context::from_env(paths)?;
    let mut out = std::io::stdout();
    let not_installed = || {
        anyhow::anyhow!(
            "the service is not installed ({} is missing); run `bricolo service install`",
            installed(init, &cx).display()
        )
    };
    let steps = match args.action {
        Action::Install(install) => {
            let target = resolve_target(init, &cx, &install)?;
            if init == Init::Systemd {
                let session = match target.session {
                    Session::Desktop => "with the graphical session",
                    Session::Headless => "at login, with no display",
                };
                println!("the node starts {session}; change it with --session");
            }
            install_plan(init, &cx, &target)
        }
        Action::Uninstall => uninstall_plan(init, &cx),
        Action::Status => return status(init, &cx, &mut System),
        Action::Start if !installed(init, &cx).exists() => return Err(not_installed()),
        Action::Start => start_plan(init, &cx),
        Action::Stop if !installed(init, &cx).exists() => return Err(not_installed()),
        Action::Stop => stop_plan(init),
    };
    apply(&steps, &mut System, &mut out, args.dry_run)?;
    Ok(ExitCode::SUCCESS)
}

/// Prints the service manager's view. Exits 2 when the service is not
/// loaded, like `bricolo status` when no node answers.
fn status(init: Init, cx: &Context, runner: &mut dyn Runner) -> Result<ExitCode> {
    let file = installed(init, cx);
    if !file.exists() {
        println!("not installed: {} is missing", file.display());
        return Ok(ExitCode::from(crate::NOT_RUNNING));
    }
    println!("installed: {}", file.display());
    let argv: Vec<String> = match init {
        Init::Launchd { uid } => vec!["launchctl".into(), "print".into(), service(uid, LABEL)],
        Init::Systemd => ["systemctl", "--user", "status", "--no-pager", UNIT]
            .map(String::from)
            .to_vec(),
        Init::TaskScheduler => ["schtasks", "/Query", "/TN", TASK, "/V", "/FO", "LIST"]
            .map(String::from)
            .to_vec(),
    };
    let ran = runner
        .run(&argv)
        .with_context(|| format!("cannot run {}", argv[0]))?;
    match init {
        Init::Launchd { .. } if ran.ok => {
            // The full answer is long; these lines say what state it is in.
            // The service's own keys sit one tab in; deeper ones belong to
            // nested sections.
            let top = ran.output.lines().filter_map(|l| l.strip_prefix('\t'));
            for line in top.filter(|l| {
                ["state =", "pid =", "program =", "last exit code ="]
                    .iter()
                    .any(|k| l.starts_with(k))
            }) {
                println!("{line}");
            }
        }
        Init::Launchd { .. } => println!("not loaded: bricolo service start loads it"),
        // systemctl status exits 3 for an inactive unit.
        Init::Systemd => print!("{}", ran.output),
        Init::TaskScheduler => print!("{}", task_summary(&ran.output)),
    }
    Ok(if ran.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(crate::NOT_RUNNING)
    })
}

/// The lines of `schtasks /Query /V /FO LIST` that say what state the
/// task is in; all of them when none match (Windows in another language).
fn task_summary(output: &str) -> String {
    let keys = ["Status:", "Last Run Time:", "Last Result:", "Task To Run:"];
    let picked: String = output
        .lines()
        .filter(|l| keys.iter().any(|k| l.starts_with(k)))
        .map(|l| format!("{l}\n"))
        .collect();
    if picked.is_empty() {
        output.to_string()
    } else {
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(root: &Path) -> Context {
        Context {
            user: "PC\\ada".into(),
            home: root.join("home"),
            paths: Paths::new(root.join("config"), root.join("data dir")),
            env: vec![(DATA_ENV.into(), root.join("data dir").display().to_string())],
            xdg_config: root.join("home/.config"),
            xdg_data: root.join("home/.local/share"),
        }
    }

    fn target(root: &Path, session: Session) -> Target {
        Target {
            bricolo: root.join("bin/bricolo"),
            ui: Some(root.join("bin/bricolo-ui")),
            app: None,
            session,
        }
    }

    const MAC: Init = Init::Launchd { uid: 501 };

    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
        fail: Vec<&'static str>,
    }

    impl Runner for Fake {
        fn run(&mut self, argv: &[String]) -> std::io::Result<Ran> {
            let line = argv.join(" ");
            let ok = !self.fail.iter().any(|f| line.starts_with(f));
            self.calls.push(line);
            Ok(Ran {
                ok,
                output: if ok { String::new() } else { "nope".into() },
            })
        }

        fn pause(&mut self) {}
    }

    #[test]
    fn the_launch_agent_runs_bricolo_run_and_restarts_it_after_a_crash() {
        let cx = context(Path::new("/r"));
        let mut t = target(Path::new("/r"), Session::Desktop);
        t.app = Some("/Applications/bricolo.app".into());
        t.bricolo = Path::new("/Applications/bricolo.app").join(NODE_IN_APP);
        let plist = launchd_plist(&cx, &t);
        assert!(plist.contains("<key>Label</key>\n\t<string>com.pierre-mike.bricolo</string>"));
        assert!(plist.contains(&format!(
            "<array>\n\t\t<string>{}</string>\n\t\t<string>run</string>\n\t</array>",
            t.bricolo.display()
        )));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(plist.contains("<key>AssociatedBundleIdentifiers</key>"));
        // Built with `join`, like the product: on Windows these paths mix `\`.
        let data = Path::new("/r").join("data dir");
        let log = data.join("logs").join("bricolo-service.log");
        assert!(plist.contains(&format!("<string>{}</string>", log.display())));
        assert!(plist.contains(&format!(
            "<key>BRICOLO_DATA_DIR</key>\n\t\t<string>{}</string>",
            data.display()
        )));
        assert!(!plist.contains("BRICOLO_CONFIG_DIR"));
        assert!(plist.contains(&format!(
            "<string>{}:{LAUNCHD_PATH}</string>",
            Path::new("/r").join("home").join(".local/bin").display()
        )));
    }

    #[test]
    fn plist_strings_are_escaped() {
        let cx = context(Path::new("/r"));
        let mut t = target(Path::new("/r"), Session::Desktop);
        t.bricolo = "/a&b/<bricolo>".into();
        let plist = launchd_plist(&cx, &t);
        assert!(plist.contains("<string>/a&amp;b/&lt;bricolo&gt;</string>"));
        assert!(!plist.contains("AssociatedBundleIdentifiers"));
    }

    #[test]
    fn the_ui_agent_opens_the_menu_bar_item_and_is_not_kept_alive() {
        let cx = context(Path::new("/r"));
        let t = target(Path::new("/r"), Session::Desktop);
        let plist = launchd_ui_plist(&cx, &t, Path::new("/r/bin/bricolo-ui"));
        assert!(plist.contains("<string>com.pierre-mike.bricolo.ui</string>"));
        assert!(
            plist.contains("<string>/r/bin/bricolo-ui</string>\n\t\t<string>--background</string>")
        );
        assert!(!plist.contains("KeepAlive"));
    }

    #[test]
    fn the_systemd_unit_follows_the_session() {
        let cx = context(Path::new("/r"));
        let desktop = systemd_unit(&cx, &target(Path::new("/r"), Session::Desktop));
        // The unit quotes a backslash, so a Windows host's paths are escaped.
        assert!(desktop.contains(&format!(
            "ExecStart={} run\n",
            systemd_quote(&Path::new("/r").join("bin/bricolo").display().to_string())
        )));
        assert!(desktop.contains("Restart=on-failure\n"));
        assert!(desktop.contains("PartOf=graphical-session.target\n"));
        assert!(desktop.contains("WantedBy=graphical-session.target\n"));
        assert!(desktop.contains("PassEnvironment=DISPLAY WAYLAND_DISPLAY XAUTHORITY"));
        let data = Path::new("/r").join("data dir").display().to_string();
        assert!(desktop.contains(&format!(
            "Environment={}\n",
            systemd_quote(&format!("BRICOLO_DATA_DIR={data}"))
        )));
        let headless = systemd_unit(&cx, &target(Path::new("/r"), Session::Headless));
        assert!(!headless.contains("graphical-session"));
        assert!(headless.contains("WantedBy=default.target\n"));
    }

    #[test]
    fn systemd_and_desktop_quoting() {
        assert_eq!(systemd_quote("/usr/bin/bricolo"), "/usr/bin/bricolo");
        assert_eq!(systemd_quote("/my apps/bricolo"), "\"/my apps/bricolo\"");
        assert_eq!(systemd_quote("/a%b$c"), "\"/a%%b$$c\"");
        assert_eq!(desktop_quote("/opt/bricolo-ui"), "/opt/bricolo-ui");
        assert_eq!(
            desktop_quote("/my apps/bricolo$ui"),
            "\"/my apps/bricolo\\$ui\""
        );
        let entry = desktop_entry(Path::new("/opt/bricolo-ui"), true);
        assert!(entry.contains("Exec=/opt/bricolo-ui --background\n"));
        assert!(entry.contains("Type=Application\n"));
    }

    #[test]
    fn installing_on_macos_writes_both_agents_and_bootstraps_them() {
        let root = tempfile::tempdir().unwrap();
        let cx = context(root.path());
        let t = target(root.path(), Session::Desktop);
        let steps = install_plan(MAC, &cx, &t);
        let mut fake = Fake::default();
        let mut out = Vec::new();
        apply(&steps, &mut fake, &mut out, false).unwrap();
        let node = cx.launch_agent(LABEL);
        let ui = cx.launch_agent(UI_LABEL);
        assert!(
            std::fs::read_to_string(&node)
                .unwrap()
                .contains("<string>run</string>")
        );
        assert!(
            std::fs::read_to_string(&ui)
                .unwrap()
                .contains("--background")
        );
        assert!(cx.paths.logs_dir().is_dir());
        assert_eq!(
            fake.calls,
            [
                format!("launchctl bootout gui/501/{LABEL}"),
                format!("launchctl enable gui/501/{LABEL}"),
                format!("launchctl bootstrap gui/501 {}", node.display()),
                format!("launchctl bootout gui/501/{UI_LABEL}"),
                format!("launchctl enable gui/501/{UI_LABEL}"),
                format!("launchctl bootstrap gui/501 {}", ui.display()),
            ]
        );
    }

    #[test]
    fn a_failed_bootout_is_fine_and_a_failed_bootstrap_is_retried_then_reported() {
        let root = tempfile::tempdir().unwrap();
        let cx = context(root.path());
        let mut t = target(root.path(), Session::Desktop);
        t.ui = None;
        let mut fake = Fake {
            fail: vec!["launchctl bootout", "launchctl bootstrap"],
            ..Fake::default()
        };
        let mut out = Vec::new();
        let err = apply(&install_plan(MAC, &cx, &t), &mut fake, &mut out, false).unwrap_err();
        assert!(
            err.to_string().contains("launchctl bootstrap gui/501"),
            "{err}"
        );
        let bootstraps = fake
            .calls
            .iter()
            .filter(|c| c.contains("bootstrap"))
            .count();
        assert_eq!(bootstraps, 11);
        assert!(String::from_utf8(out).unwrap().contains("(ignored) nope"));
    }

    #[test]
    fn installing_on_linux_writes_the_unit_and_the_desktop_entries() {
        let root = tempfile::tempdir().unwrap();
        let cx = context(root.path());
        let t = target(root.path(), Session::Headless);
        let mut fake = Fake::default();
        let mut out = Vec::new();
        apply(
            &install_plan(Init::Systemd, &cx, &t),
            &mut fake,
            &mut out,
            false,
        )
        .unwrap();
        let unit = std::fs::read_to_string(cx.unit_file()).unwrap();
        assert!(unit.contains("WantedBy=default.target"));
        assert!(
            cx.unit_file()
                .ends_with("home/.config/systemd/user/bricolo.service")
        );
        let menu = std::fs::read_to_string(cx.desktop_entry()).unwrap();
        assert!(!menu.contains("--background"));
        let autostart = std::fs::read_to_string(cx.autostart_entry()).unwrap();
        assert!(autostart.contains("--background"));
        assert_eq!(
            fake.calls,
            [
                "systemctl --user daemon-reload",
                "systemctl --user disable bricolo.service",
                "systemctl --user enable bricolo.service",
                "systemctl --user restart bricolo.service",
            ]
        );

        let mut fake = Fake::default();
        apply(
            &uninstall_plan(Init::Systemd, &cx),
            &mut fake,
            &mut out,
            false,
        )
        .unwrap();
        assert!(!cx.unit_file().exists());
        assert!(!cx.desktop_entry().exists());
        assert!(!cx.autostart_entry().exists());
        assert_eq!(
            fake.calls,
            [
                "systemctl --user disable --now bricolo.service",
                "systemctl --user daemon-reload",
            ]
        );
    }

    #[test]
    fn a_dry_run_prints_the_plan_and_touches_nothing() {
        let root = tempfile::tempdir().unwrap();
        let cx = context(root.path());
        let t = target(root.path(), Session::Desktop);
        let mut fake = Fake::default();
        let mut out = Vec::new();
        apply(&install_plan(MAC, &cx, &t), &mut fake, &mut out, true).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(fake.calls.is_empty());
        assert!(!cx.launch_agent(LABEL).exists());
        assert!(!cx.paths.logs_dir().exists());
        assert!(out.contains(&format!("would write {}", cx.launch_agent(LABEL).display())));
        assert!(out.contains("    \t<key>RunAtLoad</key>"));
        assert!(out.contains("would run: launchctl bootstrap gui/501"));
    }

    #[test]
    fn start_and_stop() {
        let cx = context(Path::new("/r"));
        let start = start_plan(MAC, &cx);
        assert_eq!(
            start.last(),
            Some(&run(
                &["launchctl", "kickstart", &format!("gui/501/{LABEL}")],
                OnFail::Stop
            ))
        );
        assert_eq!(
            stop_plan(Init::Systemd),
            [run(&["systemctl", "--user", "stop", UNIT], OnFail::Stop)]
        );
    }

    #[test]
    fn the_windows_wrapper_sets_the_folders_logs_and_restarts_after_a_crash() {
        let cx = context(Path::new("/r"));
        let mut t = target(Path::new("/r"), Session::Desktop);
        t.bricolo = r"C:\Program Files (x86)\bricolo\bricolo.exe".into();
        let cmd = task_wrapper(&cx, &t);
        assert!(cmd.starts_with("@echo off\r\n"), "{cmd}");
        assert!(
            cmd.split_inclusive('\n').all(|l| l.ends_with("\r\n")),
            "{cmd:?}"
        );
        assert!(cmd.contains("chcp 65001 >nul\r\n"));
        let data = Path::new("/r").join("data dir");
        assert!(cmd.contains(&format!("set \"BRICOLO_DATA_DIR={}\"\r\n", data.display())));
        assert!(!cmd.contains("BRICOLO_CONFIG_DIR"));
        let log = data.join("logs").join("bricolo-service.log");
        assert!(cmd.contains(&format!(
            ":run\r\n\"C:\\Program Files (x86)\\bricolo\\bricolo.exe\" run >> \"{}\" 2>&1\r\n\
             if errorlevel 1 (\r\n  ping -n 6 127.0.0.1 >nul\r\n  goto run\r\n)\r\n",
            log.display()
        )));
        assert_eq!(cmd_quote("C:\\a%b"), "\"C:\\a%%b\"");
        let ui = task_ui_wrapper(&cx, Path::new("/r/bin/bricolo-ui.exe"));
        assert!(ui.contains(&format!(
            "\"/r/bin/bricolo-ui.exe\" --background >> \"{}\" 2>&1\r\n",
            data.join("logs").join("bricolo-ui.log").display()
        )));
    }

    #[test]
    fn the_task_starts_at_this_users_logon_with_no_time_limit() {
        let cx = context(Path::new("/r"));
        let xml = task_definition(
            &cx,
            "The node & more",
            Path::new("/r/a b/bricolo-service.cmd"),
        );
        assert!(xml.contains(
            "<LogonTrigger>\n      <Enabled>true</Enabled>\n      <UserId>PC\\ada</UserId>"
        ));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
        assert!(xml.contains("<Description>The node &amp; more</Description>"));
        assert!(xml.contains("<Command>conhost.exe</Command>"));
        assert!(xml.contains(
            "<Arguments>--headless cmd.exe /d /s /c &quot;&quot;/r/a b/bricolo-service.cmd&quot;&quot;</Arguments>"
        ));
    }

    const WINDOWS: Init = Init::TaskScheduler;

    #[test]
    fn installing_on_windows_registers_both_tasks_and_runs_the_node() {
        let root = tempfile::tempdir().unwrap();
        let cx = context(root.path());
        let t = target(root.path(), Session::Desktop);
        let mut fake = Fake::default();
        let mut out = Vec::new();
        apply(&install_plan(WINDOWS, &cx, &t), &mut fake, &mut out, false).unwrap();
        let wrapper = std::fs::read_to_string(cx.task_wrapper(TASK)).unwrap();
        assert!(wrapper.contains(" run >> "));
        let ui = std::fs::read_to_string(cx.task_wrapper(UI_TASK)).unwrap();
        assert!(ui.contains("--background"));
        let xml = std::fs::read(cx.task_xml(TASK)).unwrap();
        assert_eq!(xml[..2], [0xff, 0xfe], "a UTF-16LE byte order mark");
        let units: Vec<u16> = xml[2..]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let xml = String::from_utf16(&units).unwrap();
        assert!(xml.contains(&format!("{}&quot;&quot;", cx.task_wrapper(TASK).display())));
        assert!(cx.paths.logs_dir().is_dir());
        let node_xml = cx.task_xml(TASK).display().to_string();
        let ui_xml = cx.task_xml(UI_TASK).display().to_string();
        assert_eq!(
            fake.calls,
            [
                "schtasks /End /TN bricolo".to_string(),
                format!("schtasks /Create /TN bricolo /XML {node_xml} /F"),
                "schtasks /End /TN bricolo-ui".to_string(),
                format!("schtasks /Create /TN bricolo-ui /XML {ui_xml} /F"),
                "schtasks /Run /TN bricolo".to_string(),
            ]
        );

        // A task that does not exist yet is fine to end and delete.
        let mut fake = Fake {
            fail: vec!["schtasks /End", "schtasks /Delete"],
            ..Fake::default()
        };
        apply(&uninstall_plan(WINDOWS, &cx), &mut fake, &mut out, false).unwrap();
        assert!(!cx.task_wrapper(TASK).exists());
        assert!(!cx.task_xml(TASK).exists());
        assert!(!cx.task_wrapper(UI_TASK).exists());
        assert_eq!(
            fake.calls,
            [
                "schtasks /End /TN bricolo-ui",
                "schtasks /Delete /TN bricolo-ui /F",
                "schtasks /End /TN bricolo",
                "schtasks /Delete /TN bricolo /F",
            ]
        );
    }

    #[test]
    fn a_failed_task_registration_is_reported() {
        let root = tempfile::tempdir().unwrap();
        let cx = context(root.path());
        let mut t = target(root.path(), Session::Desktop);
        t.ui = None;
        let mut fake = Fake {
            fail: vec!["schtasks /Create"],
            ..Fake::default()
        };
        let mut out = Vec::new();
        let err = apply(&install_plan(WINDOWS, &cx, &t), &mut fake, &mut out, false).unwrap_err();
        assert!(
            err.to_string().contains("schtasks /Create /TN bricolo"),
            "{err}"
        );
        assert!(!fake.calls.iter().any(|c| c.contains("/Run")));
    }

    #[test]
    fn starting_and_stopping_the_task() {
        let cx = context(Path::new("/r"));
        assert_eq!(
            start_plan(WINDOWS, &cx),
            [run(&["schtasks", "/Run", "/TN", TASK], OnFail::Stop)]
        );
        assert_eq!(
            stop_plan(WINDOWS),
            [run(&["schtasks", "/End", "/TN", TASK], OnFail::Stop)]
        );
        assert_eq!(installed(WINDOWS, &cx), cx.task_wrapper(TASK));
    }

    #[test]
    fn the_task_status_keeps_the_state_lines() {
        let listed = "\r\nFolder: \\\r\nHostName: PC\r\nTaskName: \\bricolo\r\nStatus: Running\r\nLast Run Time: 1/2/2026\r\nLast Result: 267009\r\nAuthor: PC\\ada\r\n";
        let summary = task_summary(&listed.replace('\r', ""));
        assert_eq!(
            summary,
            "Status: Running\nLast Run Time: 1/2/2026\nLast Result: 267009\n"
        );
        assert_eq!(task_summary("Statut : En cours\n"), "Statut : En cours\n");
    }

    #[test]
    fn find_app_prefers_applications_then_the_home_folder() {
        let home = tempfile::tempdir().unwrap();
        // /Applications may hold a real install on a dev machine.
        if Path::new("/Applications").join(APP_NAME).exists() {
            return;
        }
        assert_eq!(find_app(home.path()), None);
        let app = home.path().join("Applications").join(APP_NAME);
        std::fs::create_dir_all(app_node(&app).parent().unwrap()).unwrap();
        std::fs::write(app_node(&app), "").unwrap();
        assert_eq!(find_app(home.path()), Some(app));
    }
}
