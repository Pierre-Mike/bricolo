//! `app` on Linux. `launch` finds a desktop file by id ("firefox",
//! "org.gnome.Nautilus.desktop") or by its `Name`, and starts it with
//! `gio launch` or `gtk-launch`, or runs its `Exec` line itself; with no
//! desktop file it runs a program of that name from `PATH`. If one of the
//! app's windows is open, it raises that window instead, as `open -a`
//! does. `open-url` runs `xdg-open`. `running` and `frontmost` read the
//! window list, from Hyprland's IPC socket under Hyprland, else from X11.
//!
//! An app on X11 is the set of its windows: those whose `WM_CLASS`
//! instance or class is the name asked for, or one its desktop file gives
//! (see `Found::window_classes`), in any case. `quit` asks the window
//! manager to close each of them (`_NET_CLOSE_WINDOW`, as their close
//! buttons do), so the app can save or ask first; it never kills the
//! process. `set-hidden` minimizes every window, or restores them all and
//! leaves the newest in front. Both answer `failed` with "no such app"
//! when no window matches, as on macOS. They need an X11 session.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::which;
use super::x11::{X11, XError};
use crate::app::{AppInfo, AppProvider, check_app, check_url};
use crate::caller::Caller;
use crate::error::CallError;

/// `xdg-open`, `gio launch` and `gtk-launch` hand the work to the app and
/// return. One still running after this is left to run.
const LAUNCH_WAIT: Duration = Duration::from_secs(10);

/// The `[Desktop Entry]` keys `launch` uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub name: Option<String>,
    pub exec: Option<String>,
    pub wm_class: Option<String>,
    /// `Hidden=true`: the file deletes the entry, as if it did not exist.
    pub hidden: bool,
    /// `Type=Application`, the default when the key is missing.
    pub application: bool,
}

/// Reads the `[Desktop Entry]` group of a desktop file. Localized keys
/// (`Name[fr]`) are skipped; values have their `\s`, `\n`, `\t`, `\r` and
/// `\\` escapes undone.
pub fn parse_entry(text: &str) -> Entry {
    let mut entry = Entry {
        application: true,
        ..Entry::default()
    };
    let mut in_group = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unescape(value.trim());
        match key.trim() {
            "Name" => entry.name = Some(value),
            "Exec" => entry.exec = Some(value),
            "StartupWMClass" => entry.wm_class = Some(value),
            "Hidden" => entry.hidden = value == "true",
            "Type" => entry.application = value == "Application",
            _ => {}
        }
    }
    entry
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits an `Exec` value into arguments and drops the field codes (`%f`,
/// `%U` and the rest), since `launch` passes no files or URLs. Quoting
/// follows the desktop entry spec: double quotes, with `\"`, `` \` ``,
/// `\$` and `\\` inside them.
pub fn exec_argv(exec: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' => match chars.next() {
                Some(next) => {
                    cur.push(next);
                    started = true;
                }
                None => return Err(format!("{exec:?} ends with a backslash")),
            },
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            '%' if !quoted => match chars.next() {
                Some('%') => {
                    cur.push('%');
                    started = true;
                }
                Some(_) => {}
                None => return Err(format!("{exec:?} ends with %")),
            },
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err(format!("{exec:?} has an unclosed quote"));
    }
    if started {
        args.push(cur);
    }
    // A field code alone, such as `%U`, leaves nothing; drop empty words
    // that did not come from quotes.
    if args.is_empty() {
        return Err(format!("{exec:?} names no program"));
    }
    Ok(args)
}

/// The `applications` folders, most important first: `$XDG_DATA_HOME`
/// (default `~/.local/share`), then each of `$XDG_DATA_DIRS` (default
/// `/usr/local/share:/usr/share`).
pub fn application_dirs(
    data_home: Option<&OsStr>,
    home: Option<&Path>,
    data_dirs: Option<&OsStr>,
) -> Vec<PathBuf> {
    fn set(v: Option<&OsStr>) -> Option<&OsStr> {
        v.filter(|v| !v.is_empty())
    }
    let mut dirs = Vec::new();
    match (set(data_home), home) {
        (Some(dir), _) => dirs.push(PathBuf::from(dir)),
        (None, Some(home)) => dirs.push(home.join(".local/share")),
        (None, None) => {}
    }
    let system = set(data_dirs).unwrap_or(OsStr::new("/usr/local/share:/usr/share"));
    dirs.extend(std::env::split_paths(system));
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

fn current_dirs() -> Vec<PathBuf> {
    application_dirs(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref().map(Path::new),
        std::env::var_os("XDG_DATA_DIRS").as_deref(),
    )
}

/// Every desktop file under `dirs` with its id: the path below the folder
/// with `/` turned into `-`. The first folder with an id wins.
pub fn desktop_files(dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    fn walk(base: &Path, dir: &Path, depth: u8, out: &mut Vec<(String, PathBuf)>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                if depth < 4 {
                    walk(base, &path, depth + 1, out);
                }
            } else if path.extension() == Some(OsStr::new("desktop")) {
                let Ok(rel) = path.strip_prefix(base) else {
                    continue;
                };
                let id = rel.to_string_lossy().replace('/', "-");
                if !out.iter().any(|(seen, _)| *seen == id) {
                    out.push((id, path));
                }
            }
        }
    }
    let mut out = Vec::new();
    for dir in dirs {
        walk(dir, dir, 0, &mut out);
    }
    out
}

/// A desktop file `launch` resolved, with its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub path: PathBuf,
    pub entry: Entry,
}

/// Finds the desktop file for `app`: by id first (`app` or
/// `app.desktop`, any case), then by `Name`, `StartupWMClass` or the
/// program in `Exec`, any case. Hidden entries and non-applications don't
/// count.
pub fn resolve(app: &str, dirs: &[PathBuf]) -> Option<Found> {
    let files = desktop_files(dirs);
    let wanted = app.strip_suffix(".desktop").unwrap_or(app);
    let read = |id: &String, path: &PathBuf| {
        let text = std::fs::read_to_string(path).ok()?;
        let entry = parse_entry(&text);
        (entry.application && !entry.hidden).then(|| Found {
            id: id.clone(),
            path: path.clone(),
            entry,
        })
    };
    let by_id = files.iter().find_map(|(id, path)| {
        let stem = id.strip_suffix(".desktop").unwrap_or(id);
        if stem.eq_ignore_ascii_case(wanted) {
            read(id, path)
        } else {
            None
        }
    });
    if by_id.is_some() {
        return by_id;
    }
    let all: Vec<Found> = files.iter().filter_map(|(id, p)| read(id, p)).collect();
    let same = |v: &Option<String>| v.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(app));
    all.iter()
        .find(|f| same(&f.entry.name))
        .or_else(|| all.iter().find(|f| same(&f.entry.wm_class)))
        .or_else(|| all.iter().find(|f| same(&f.program())))
        .cloned()
}

impl Found {
    /// The file name of the program in `Exec`.
    pub fn program(&self) -> Option<String> {
        let argv = exec_argv(self.entry.exec.as_deref()?).ok()?;
        let name = Path::new(argv.first()?).file_name()?;
        Some(name.to_string_lossy().into_owned())
    }

    /// Names one of the app's windows may carry in `WM_CLASS`: the
    /// entry's `StartupWMClass` when it has one, which is what the spec
    /// says to match; otherwise its program, id and `Name`.
    fn window_classes(&self) -> Vec<String> {
        if let Some(class) = &self.entry.wm_class {
            return vec![class.clone()];
        }
        let id = self.id.strip_suffix(".desktop").unwrap_or(&self.id);
        [
            self.program(),
            Some(id.to_string()),
            self.entry.name.clone(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// Runs a launcher and waits up to `LAUNCH_WAIT` for it to hand over.
async fn run_launcher(program: &Path, args: &[&OsStr]) -> Result<(), CallError> {
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| CallError::Failed(format!("cannot run {}: {e}", program.display())))?;
    match tokio::time::timeout(LAUNCH_WAIT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(exited(program, status)),
        Ok(Err(e)) => Err(CallError::failed(e)),
        // Still running: some browsers keep `xdg-open` waiting. Leave it.
        Err(_) => {
            tokio::spawn(async move { child.wait().await });
            Ok(())
        }
    }
}

fn exited(program: &Path, status: ExitStatus) -> CallError {
    CallError::Failed(format!("{} failed: {status}", program.display()))
}

/// Starts `argv` in its own process group and lets it run.
fn spawn_detached(argv: &[String]) -> Result<(), CallError> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| CallError::Failed("nothing to run".into()))?;
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| CallError::Failed(format!("cannot run {program}: {e}")))?;
    // Reap it when it exits.
    tokio::spawn(async move { child.wait().await });
    Ok(())
}

pub struct LinuxApps {
    x: Arc<X11>,
}

impl LinuxApps {
    pub fn new(x: Arc<X11>) -> Self {
        Self { x }
    }

    /// Raises an open window whose `WM_CLASS` matches one of `names`.
    /// False when there is none, or no X11 session to look in.
    async fn raise(&self, names: Vec<String>) -> bool {
        if let Some(h) = super::hyprland::Hyprland::current() {
            let raised = super::hyprland::window::blocking(h, move |h| {
                super::hyprland::window::raise(h, &names)
            })
            .await;
            return raised.unwrap_or(false);
        }
        let raised = self
            .x
            .with(move |c| {
                for window in c.clients()? {
                    let Ok((instance, class)) = c.class(window) else {
                        continue;
                    };
                    if class_matches(&names, &instance, &class) {
                        c.focus(window)?;
                        return Ok(true);
                    }
                }
                Ok(false)
            })
            .await;
        raised.unwrap_or(false)
    }

    /// The app windows of `app`, oldest first, for `quit` and
    /// `set-hidden`. Answers `failed` when it has none.
    async fn windows_of(&self, app: &str) -> Result<Vec<u32>, CallError> {
        let app = check_app(app)?.to_string();
        let names = tokio::task::spawn_blocking(move || {
            let found = resolve(&app, &current_dirs());
            app_names(&app, found.as_ref())
        })
        .await
        .map_err(CallError::failed)?;
        let windows = self
            .x
            .with(move |c| {
                let mut classes = Vec::new();
                for window in c.clients()? {
                    // A window can close while we read it.
                    match c
                        .is_app_window(window)
                        .and_then(|app| app.then(|| c.class(window)).transpose())
                    {
                        Ok(Some(class)) => classes.push((window, class)),
                        Ok(None) | Err(XError::NotFound) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(select_windows(&classes, &names))
            })
            .await?;
        if windows.is_empty() {
            return Err(CallError::Failed("no such app".into()));
        }
        Ok(windows)
    }
}

/// Whether a window whose `WM_CLASS` is `instance` and `class` belongs to
/// one of `names`, in any case.
fn class_matches(names: &[String], instance: &str, class: &str) -> bool {
    names
        .iter()
        .any(|n| n.eq_ignore_ascii_case(instance) || n.eq_ignore_ascii_case(class))
}

/// The names whose windows make up `app`: the name itself, then the
/// classes its desktop file gives, if it has one.
fn app_names(app: &str, found: Option<&Found>) -> Vec<String> {
    let mut names = vec![app.to_string()];
    if let Some(found) = found {
        for class in found.window_classes() {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(&class)) {
                names.push(class);
            }
        }
    }
    names
}

/// The windows among `windows`, given with their `WM_CLASS`, that belong
/// to `names`, in the same order.
fn select_windows<W: Copy>(windows: &[(W, (String, String))], names: &[String]) -> Vec<W> {
    windows
        .iter()
        .filter(|(_, (instance, class))| class_matches(names, instance, class))
        .map(|(window, _)| *window)
        .collect()
}

/// The app that owns `window`: its `WM_CLASS` class and process id (0 when
/// the window does not say).
fn app_info(c: &super::x11::Conn, window: u32) -> Result<AppInfo, XError> {
    let (_, class) = c.class(window)?;
    let pid = c.pid(window)?.and_then(|p| i32::try_from(p).ok());
    Ok(AppInfo {
        name: class,
        bundle_id: None,
        pid: pid.unwrap_or(0),
    })
}

#[async_trait]
impl AppProvider for LinuxApps {
    /// App events are not implemented on Linux.
    async fn watch(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn launch(&self, _: &Caller, app: String) -> Result<(), CallError> {
        let app = check_app(&app)?.to_string();
        let name = app.clone();
        let found = tokio::task::spawn_blocking(move || resolve(&name, &current_dirs()))
            .await
            .map_err(CallError::failed)?;
        let names = match &found {
            Some(found) => found.window_classes(),
            None => vec![app.clone()],
        };
        if self.raise(names).await {
            return Ok(());
        }
        let Some(found) = found else {
            return match which(&app) {
                Some(path) => spawn_detached(&[path.to_string_lossy().into_owned()]),
                None => Err(CallError::Failed(format!(
                    "no desktop file or program named {app}"
                ))),
            };
        };
        if let Some(gio) = which("gio") {
            let args = [OsStr::new("launch"), found.path.as_os_str()];
            return run_launcher(&gio, &args).await;
        }
        if let Some(gtk) = which("gtk-launch") {
            return run_launcher(&gtk, &[OsStr::new(&found.id)]).await;
        }
        let exec = found
            .entry
            .exec
            .as_deref()
            .ok_or_else(|| CallError::Failed(format!("{} has no Exec line", found.id)))?;
        spawn_detached(&exec_argv(exec).map_err(CallError::Failed)?)
    }

    async fn open_url(&self, _: &Caller, url: String) -> Result<(), CallError> {
        check_url(&url)?;
        let Some(xdg_open) = which("xdg-open") else {
            tracing::debug!("app: xdg-open is not installed");
            return Err(CallError::Unavailable);
        };
        run_launcher(&xdg_open, &[OsStr::new(&url)]).await
    }

    async fn running(&self, _: &Caller) -> Result<Vec<AppInfo>, CallError> {
        if let Some(h) = super::hyprland::Hyprland::current() {
            return super::hyprland::window::blocking(h, super::hyprland::window::running).await;
        }
        self.x
            .with(|c| {
                let mut apps: Vec<AppInfo> = Vec::new();
                for window in c.clients()? {
                    let info = match c.is_app_window(window) {
                        Ok(true) => app_info(c, window),
                        Ok(false) => continue,
                        Err(e) => Err(e),
                    };
                    match info {
                        Ok(info) => {
                            if !apps
                                .iter()
                                .any(|a| a.pid == info.pid && a.name == info.name)
                            {
                                apps.push(info);
                            }
                        }
                        Err(XError::NotFound) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(apps)
            })
            .await
    }

    async fn frontmost(&self, _: &Caller) -> Result<Option<AppInfo>, CallError> {
        if let Some(h) = super::hyprland::Hyprland::current() {
            return super::hyprland::window::blocking(h, super::hyprland::window::frontmost).await;
        }
        self.x
            .with(|c| match c.active()? {
                Some(window) => match app_info(c, window) {
                    Ok(info) => Ok(Some(info)),
                    Err(XError::NotFound) => Ok(None),
                    Err(e) => Err(e),
                },
                None => Ok(None),
            })
            .await
    }

    async fn quit(&self, _: &Caller, app: String) -> Result<(), CallError> {
        let windows = self.windows_of(&app).await?;
        self.x
            .with(move |c| windows.into_iter().try_for_each(|w| c.close(w)))
            .await
    }

    async fn set_hidden(&self, _: &Caller, app: String, hidden: bool) -> Result<(), CallError> {
        let windows = self.windows_of(&app).await?;
        // Restoring activates each window in turn, so the newest ends up
        // in front.
        self.x
            .with(move |c| {
                for window in windows {
                    match c.set_minimized(window, hidden) {
                        Ok(()) | Err(XError::NotFound) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_desktop_entry_group_only() {
        let text = "\
# comment
[Desktop Entry]
Type=Application
Name=Text Editor
Name[fr]=Éditeur
Exec=gedit %U
StartupWMClass=Gedit
Icon=gedit

[Desktop Action new-window]
Name=New Window
Exec=gedit --new-window
";
        let e = parse_entry(text);
        assert_eq!(e.name.as_deref(), Some("Text Editor"));
        assert_eq!(e.exec.as_deref(), Some("gedit %U"));
        assert_eq!(e.wm_class.as_deref(), Some("Gedit"));
        assert!(e.application && !e.hidden);
        let link = parse_entry("[Desktop Entry]\nType=Link\nHidden=true\nName=A\\sB\n");
        assert!(!link.application && link.hidden);
        assert_eq!(link.name.as_deref(), Some("A B"));
    }

    #[test]
    fn exec_lines_split_and_drop_field_codes() {
        let argv = |s: &str| exec_argv(s).unwrap();
        assert_eq!(argv("firefox %u"), ["firefox"]);
        assert_eq!(argv("code --new-window %F"), ["code", "--new-window"]);
        assert_eq!(
            argv(r#""/opt/My App/run" --name "a \"b\"" 100%%"#),
            ["/opt/My App/run", "--name", "a \"b\"", "100%"]
        );
        assert_eq!(argv("env A=1 prog --x=%f"), ["env", "A=1", "prog", "--x="]);
        assert_eq!(argv(r#"sh -c "echo \$HOME""#), ["sh", "-c", "echo $HOME"]);
        assert!(exec_argv("%U").is_err());
        assert!(exec_argv(r#"prog "open"#).is_err());
    }

    #[test]
    fn application_dirs_follow_xdg_defaults() {
        let home = Path::new("/home/me");
        assert_eq!(
            application_dirs(None, Some(home), None),
            [
                PathBuf::from("/home/me/.local/share/applications"),
                PathBuf::from("/usr/local/share/applications"),
                PathBuf::from("/usr/share/applications"),
            ]
        );
        assert_eq!(
            application_dirs(
                Some(OsStr::new("/data")),
                Some(home),
                Some(OsStr::new("/a:/b"))
            ),
            [
                PathBuf::from("/data/applications"),
                PathBuf::from("/a/applications"),
                PathBuf::from("/b/applications"),
            ]
        );
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn resolves_by_id_then_name_class_or_program() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let (u, s) = (user.path(), system.path());
        write(
            s,
            "org.gnome.Nautilus.desktop",
            "[Desktop Entry]\nName=Files\nExec=nautilus --new-window %U\n",
        );
        write(
            s,
            "firefox.desktop",
            "[Desktop Entry]\nName=Firefox\nExec=/usr/lib/firefox/firefox %u\nStartupWMClass=Navigator\n",
        );
        write(
            s,
            "kde/konsole.desktop",
            "[Desktop Entry]\nName=Konsole\nExec=konsole\n",
        );
        // The user's copy hides the system one.
        write(
            s,
            "hidden.desktop",
            "[Desktop Entry]\nName=Hidden App\nExec=hidden\n",
        );
        write(u, "hidden.desktop", "[Desktop Entry]\nHidden=true\n");
        let dirs = [u.to_path_buf(), s.to_path_buf()];

        let id = |app: &str| resolve(app, &dirs).map(|f| f.id);
        assert_eq!(
            id("org.gnome.Nautilus").as_deref(),
            Some("org.gnome.Nautilus.desktop")
        );
        assert_eq!(id("firefox.desktop").as_deref(), Some("firefox.desktop"));
        assert_eq!(id("FIREFOX").as_deref(), Some("firefox.desktop"));
        assert_eq!(id("kde-konsole").as_deref(), Some("kde-konsole.desktop"));
        assert_eq!(id("files").as_deref(), Some("org.gnome.Nautilus.desktop"));
        assert_eq!(id("navigator").as_deref(), Some("firefox.desktop"));
        assert_eq!(
            id("nautilus").as_deref(),
            Some("org.gnome.Nautilus.desktop")
        );
        assert_eq!(id("hidden"), None);
        assert_eq!(id("Hidden App"), None);
        assert_eq!(id("nothing"), None);

        let firefox = resolve("firefox", &dirs).unwrap();
        assert_eq!(firefox.program().as_deref(), Some("firefox"));
        assert_eq!(firefox.window_classes(), ["Navigator"]);
        let files = resolve("files", &dirs).unwrap();
        assert_eq!(
            files.window_classes(),
            ["nautilus", "org.gnome.Nautilus", "Files"]
        );
    }

    #[test]
    fn an_app_is_the_windows_its_names_match() {
        let class = |i: &str, c: &str| (i.to_string(), c.to_string());
        let windows = [
            (1, class("xterm", "XTerm")),
            (2, class("navigator", "firefox")),
            (3, class("xterm", "XTerm")),
            (4, class("org.gnome.Nautilus", "org.gnome.Nautilus")),
        ];
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // The instance or the class, in any case, oldest first.
        assert_eq!(select_windows(&windows, &names(&["xterm"])), [1, 3]);
        assert_eq!(select_windows(&windows, &names(&["XTERM"])), [1, 3]);
        assert_eq!(select_windows(&windows, &names(&["Navigator"])), [2]);
        assert_eq!(select_windows(&windows, &names(&["Files"])), [] as [i32; 0]);
        assert_eq!(select_windows(&windows, &names(&["xter"])), [] as [i32; 0]);

        // A desktop file adds its window classes: "Files" finds Nautilus.
        let found = Found {
            id: "org.gnome.Nautilus.desktop".into(),
            path: PathBuf::from("/x/org.gnome.Nautilus.desktop"),
            entry: parse_entry("[Desktop Entry]\nName=Files\nExec=nautilus %U\n"),
        };
        let files = app_names("Files", Some(&found));
        assert_eq!(files, ["Files", "nautilus", "org.gnome.Nautilus"]);
        assert_eq!(select_windows(&windows, &files), [4]);
        assert_eq!(app_names("xterm", None), ["xterm"]);
    }
}
