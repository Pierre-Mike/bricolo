//! Hyprland backends for `window`, `hotkey`, `keyboard` and the app list.
//!
//! Wayland has no common protocol for listing or moving other clients'
//! windows, or for global hotkeys, but Hyprland answers both over its IPC
//! socket. The node talks to `$XDG_RUNTIME_DIR/hypr/<signature>/.socket.sock`
//! for queries (`j/clients`, `j/monitors`, `j/binds`) and actions, and reads
//! `.socket2.sock` for events: the hotkey presses (`custom>>bricolo-hotkey:...`)
//! and config reloads, which drop runtime binds.
//!
//! Hyprland 0.55 and later run a Lua config, and actions go through `eval`
//! with `hl.*` calls. Older releases (and newer ones still on a
//! `hyprland.conf`) take `dispatch` and `keyword` lines. [`Dialect`] picks
//! one by asking once per Hyprland instance. See docs/linux.md.

pub mod hotkey;
pub mod keyboard;
pub mod window;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::Session;
use crate::error::CallError;
use crate::hotkey::{Chord, Modifier};
use crate::window::{Rect, Screen, WindowInfo};

/// How long one request may take before the node gives up on Hyprland.
const TIMEOUT: Duration = Duration::from_secs(2);

/// Which command language this Hyprland takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    /// `eval <lua>` with `hl.dispatch`, `hl.bind`, `hl.unbind`.
    Lua,
    /// `dispatch <name> <args>` and `keyword bind ...`.
    Legacy,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HyprError {
    #[error("cannot reach Hyprland: {0}")]
    Io(String),
    #[error("Hyprland refused {command:?}: {reply}")]
    Refused { command: String, reply: String },
    #[error("cannot read Hyprland's answer to {query}: {error}")]
    Parse { query: String, error: String },
    #[error("no such window")]
    NotFound,
}

impl From<HyprError> for CallError {
    fn from(e: HyprError) -> Self {
        CallError::Failed(e.to_string())
    }
}

/// One running Hyprland instance, found from the environment.
#[derive(Clone, Debug)]
pub struct Hyprland {
    dir: PathBuf,
    dialect: Dialect,
}

impl Hyprland {
    /// The Hyprland this node runs under: a Wayland session with
    /// `HYPRLAND_INSTANCE_SIGNATURE` set and its socket in place. Read on
    /// each call, like [`Session::current`].
    pub fn current() -> Option<Self> {
        if Session::current() != Session::Wayland {
            return None;
        }
        let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let dir = socket_dir(runtime.as_deref(), Path::new(&sig))?;
        let dialect = dialect_for(&dir);
        Some(Self { dir, dialect })
    }

    /// A client for the sockets in `dir`, for tests.
    #[cfg(test)]
    pub fn at(dir: PathBuf, dialect: Dialect) -> Self {
        Self { dir, dialect }
    }

    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// Sends one request and returns the whole reply.
    pub fn request(&self, command: &str) -> Result<String, HyprError> {
        request_at(&self.dir, command)
    }

    /// Sends an action and checks that Hyprland answered `ok` to each part.
    pub fn run(&self, command: &str) -> Result<(), HyprError> {
        let reply = self.request(command)?;
        if reply_ok(&reply) {
            Ok(())
        } else {
            Err(HyprError::Refused {
                command: command.to_string(),
                reply: reply.trim().to_string(),
            })
        }
    }

    /// A `j/` query, parsed.
    pub fn query<T: for<'de> Deserialize<'de>>(&self, query: &str) -> Result<T, HyprError> {
        let reply = self.request(&format!("j/{query}"))?;
        serde_json::from_str(&reply).map_err(|e| HyprError::Parse {
            query: query.to_string(),
            error: e.to_string(),
        })
    }

    pub fn clients(&self) -> Result<Vec<Client>, HyprError> {
        self.query("clients")
    }

    /// The focused window; Hyprland answers `{}` when there is none.
    pub fn active(&self) -> Result<Option<Client>, HyprError> {
        let value: serde_json::Value = self.query("activewindow")?;
        if value.get("address").is_none() {
            return Ok(None);
        }
        serde_json::from_value(value)
            .map(Some)
            .map_err(|e| HyprError::Parse {
                query: "activewindow".into(),
                error: e.to_string(),
            })
    }

    pub fn monitors(&self) -> Result<Vec<Monitor>, HyprError> {
        self.query("monitors")
    }

    /// Every bind, with the keycode of `code:NN` binds filled in: Hyprland
    /// 0.56 lists those with an empty key and keycode 0 in JSON, but names
    /// them in the plain-text listing.
    pub fn binds(&self) -> Result<Vec<Bind>, HyprError> {
        let mut binds: Vec<Bind> = self.query("binds")?;
        if binds.iter().any(|b| b.key.is_empty() && b.keycode == 0) {
            binds.extend(code_binds(&self.request("binds")?));
        }
        Ok(binds)
    }

    /// The client with window id `id`.
    pub fn client(&self, id: u64) -> Result<Client, HyprError> {
        self.clients()?
            .into_iter()
            .find(|c| c.mapped && c.id() == Some(id))
            .ok_or(HyprError::NotFound)
    }

    /// Opens the event socket, `.socket2.sock`.
    pub fn events(&self) -> Result<BufReader<UnixStream>, HyprError> {
        let stream = UnixStream::connect(self.dir.join(".socket2.sock"))
            .map_err(|e| HyprError::Io(e.to_string()))?;
        Ok(BufReader::new(stream))
    }

    /// Runs `expr`, a Lua expression, inside Hyprland and returns its value
    /// as text. `eval` itself answers only `ok`, so the expression's value
    /// comes back as a custom event on the event socket. Lua dialect only.
    pub fn ask(&self, expr: &str) -> Result<String, HyprError> {
        let mut events = self.events()?;
        events
            .get_ref()
            .set_read_timeout(Some(TIMEOUT))
            .map_err(|e| HyprError::Io(e.to_string()))?;
        let nonce = format!("{}-{}", std::process::id(), next_nonce());
        let prefix = format!("custom>>bricolo-reply:{nonce}:");
        self.run(&format!(
            "eval hl.dispatch(hl.dsp.event({} .. tostring({expr})))",
            lua_quote(&format!("bricolo-reply:{nonce}:"))
        ))?;
        let deadline = Instant::now() + TIMEOUT;
        let mut line = String::new();
        while Instant::now() < deadline {
            line.clear();
            match events.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(value) = line.trim_end().strip_prefix(&prefix) {
                        return Ok(value.to_string());
                    }
                }
                Err(e) => return Err(HyprError::Io(e.to_string())),
            }
        }
        Err(HyprError::Io("no answer on the event socket".into()))
    }
}

fn next_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The folder that holds the sockets: `$XDG_RUNTIME_DIR/hypr/<sig>` since
/// Hyprland 0.40, `/tmp/hypr/<sig>` before.
pub fn socket_dir(runtime: Option<&Path>, sig: &Path) -> Option<PathBuf> {
    let candidates = runtime
        .map(|r| r.join("hypr").join(sig))
        .into_iter()
        .chain([Path::new("/tmp/hypr").join(sig)]);
    candidates
        .into_iter()
        .find(|d| d.join(".socket.sock").exists())
}

fn request_at(dir: &Path, command: &str) -> Result<String, HyprError> {
    let io = |e: std::io::Error| HyprError::Io(e.to_string());
    let mut stream = UnixStream::connect(dir.join(".socket.sock")).map_err(io)?;
    stream.set_read_timeout(Some(TIMEOUT)).map_err(io)?;
    stream.set_write_timeout(Some(TIMEOUT)).map_err(io)?;
    stream.write_all(command.as_bytes()).map_err(io)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).map_err(io)?;
    Ok(reply)
}

/// The dialect of the Hyprland at `dir`, asked once and remembered.
fn dialect_for(dir: &Path) -> Dialect {
    static SEEN: OnceLock<Mutex<HashMap<PathBuf, Dialect>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Mutex::default);
    if let Some(d) = seen.lock().unwrap().get(dir) {
        return *d;
    }
    let dialect = match request_at(dir, "eval return 1") {
        Ok(reply) if reply.trim() == "ok" => Dialect::Lua,
        Ok(_) => Dialect::Legacy,
        // Not remembered: Hyprland may still be starting.
        Err(_) => return Dialect::Legacy,
    };
    seen.lock().unwrap().insert(dir.to_path_buf(), dialect);
    dialect
}

/// Whether every part of a reply is `ok`. A `[[BATCH]]` answers one `ok`
/// per command, separated by blank lines.
pub fn reply_ok(reply: &str) -> bool {
    let mut parts = reply.split('\n').map(str::trim).filter(|p| !p.is_empty());
    let first = parts.next();
    first == Some("ok") && parts.all(|p| p == "ok")
}

/// `text` as a Lua string literal, safe to splice into `eval`.
pub fn lua_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\x7f' => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A window as `j/clients` lists it. Fields Hyprland may leave out (or
/// that older releases name differently) default.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Client {
    pub address: String,
    pub mapped: bool,
    pub hidden: bool,
    pub at: [i32; 2],
    pub size: [i32; 2],
    pub class: String,
    pub title: String,
    pub pid: i32,
    pub floating: bool,
    /// An integer mode since 0.33, a bool before.
    pub fullscreen: serde_json::Value,
    pub workspace: Workspace,
}

/// A workspace as `j/clients` and `j/activeworkspace` give it. Special
/// workspaces have negative ids and names that start with `special:`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
}

/// Where `window.set-minimized` parks a window: Hyprland has no
/// minimized state, so a special workspace, hidden until toggled, stands
/// in for one.
pub const MINIMIZED: &str = "special:bricolo-minimized";

impl Client {
    /// Parked by `window.set-minimized`.
    pub fn is_minimized(&self) -> bool {
        self.workspace.name == MINIMIZED
    }

    /// The window id: the address as a number.
    pub fn id(&self) -> Option<u64> {
        parse_address(&self.address)
    }

    pub fn is_fullscreen(&self) -> bool {
        match &self.fullscreen {
            serde_json::Value::Bool(b) => *b,
            serde_json::Value::Number(n) => n.as_i64().is_some_and(|n| n != 0),
            _ => false,
        }
    }

    pub fn frame(&self) -> Rect {
        Rect {
            x: self.at[0],
            y: self.at[1],
            width: self.size[0].max(0) as u32,
            height: self.size[1].max(0) as u32,
        }
    }

    pub fn info(&self) -> Option<WindowInfo> {
        Some(WindowInfo {
            id: self.id()?,
            app: self.class.clone(),
            title: self.title.clone(),
            frame: self.frame(),
        })
    }

    /// Shown to the user: mapped, not a hidden group member, and with a
    /// class or a title.
    pub fn is_app_window(&self) -> bool {
        self.mapped && !self.hidden && !(self.class.is_empty() && self.title.is_empty())
    }
}

/// `0x55fe1313a980` as a number.
pub fn parse_address(address: &str) -> Option<u64> {
    let hex = address.strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

/// A window id back as the selector Hyprland takes.
pub fn address(id: u64) -> String {
    format!("address:0x{id:x}")
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Monitor {
    pub id: i64,
    pub x: i32,
    pub y: i32,
    /// In pixels; the layout uses pixels divided by `scale`.
    pub width: i32,
    pub height: i32,
    pub scale: f64,
    pub transform: i32,
    /// Space panels keep for themselves: left, top, right, bottom.
    pub reserved: [i32; 4],
    pub focused: bool,
    pub disabled: bool,
}

impl Monitor {
    /// The monitor in layout coordinates, which windows use too.
    pub fn screen(&self) -> Screen {
        let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (mut w, mut h) = (
            (f64::from(self.width) / scale).round() as i32,
            (f64::from(self.height) / scale).round() as i32,
        );
        // Transforms 1, 3, 5 and 7 turn the monitor a quarter.
        if self.transform % 2 == 1 {
            std::mem::swap(&mut w, &mut h);
        }
        let [left, top, right, bottom] = self.reserved;
        let frame = Rect {
            x: self.x,
            y: self.y,
            width: w.max(0) as u32,
            height: h.max(0) as u32,
        };
        let visible_frame = Rect {
            x: self.x + left,
            y: self.y + top,
            width: (w - left - right).max(0) as u32,
            height: (h - top - bottom).max(0) as u32,
        };
        Screen {
            id: u32::try_from(self.id).unwrap_or(0),
            frame,
            visible_frame,
            main: self.focused,
        }
    }
}

/// The screens, with the focused monitor as `main` (Hyprland has no
/// primary monitor). If none says it is focused, the first is main.
pub fn screens(monitors: &[Monitor]) -> Vec<Screen> {
    let mut out: Vec<Screen> = monitors
        .iter()
        .filter(|m| !m.disabled)
        .map(Monitor::screen)
        .collect();
    if !out.iter().any(|s| s.main)
        && let Some(first) = out.first_mut()
    {
        first.main = true;
    }
    out
}

/// One entry of `j/binds`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Bind {
    pub modmask: u32,
    pub submap: String,
    pub key: String,
    pub keycode: u32,
    pub dispatcher: String,
    pub arg: String,
    pub description: String,
    pub mouse: bool,
}

/// The `code:NN` binds of the plain-text `binds` listing, where each bind
/// is a block of `name: value` lines.
pub fn code_binds(text: &str) -> Vec<Bind> {
    let mut out = Vec::new();
    let mut current = Bind::default();
    let mut code = None;
    let mut flush = |b: &mut Bind, code: &mut Option<u32>| {
        if let Some(c) = code.take() {
            out.push(Bind {
                keycode: c,
                ..std::mem::take(b)
            });
        } else {
            *b = Bind::default();
        }
    };
    for line in text.lines() {
        let Some((name, value)) = line.trim().split_once(':') else {
            if !line.trim().is_empty() {
                // A block starts with the bind's kind ("bind", "bindd").
                flush(&mut current, &mut code);
            }
            continue;
        };
        let value = value.trim();
        match name {
            "modmask" => current.modmask = value.parse().unwrap_or(0),
            "submap" => current.submap = value.to_string(),
            "key" => {
                code = value
                    .rsplit(" + ")
                    .next()
                    .and_then(|k| k.strip_prefix("code:"))
                    .and_then(|n| n.trim().parse().ok());
            }
            "description" => current.description = value.to_string(),
            _ => {}
        }
    }
    flush(&mut current, &mut code);
    out
}

pub const SHIFT: u32 = 1;
pub const CTRL: u32 = 4;
pub const ALT: u32 = 8;
pub const SUPER: u32 = 64;

/// A chord as Hyprland names it: a modifier mask (the X11 bits) and an xkb
/// key name, with the keycode the key has on a standard (evdev, pc105)
/// keyboard, so binds written as `code:NN` count as conflicts too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Keys {
    pub mods: u32,
    pub key: &'static str,
    pub keycode: u32,
}

impl Keys {
    pub fn from_chord(chord: &Chord) -> Option<Self> {
        let mods = chord
            .mods
            .iter()
            .map(|m| match m {
                Modifier::Shift => SHIFT,
                Modifier::Ctrl => CTRL,
                Modifier::Alt => ALT,
                Modifier::Cmd => SUPER,
            })
            .fold(0, |a, b| a | b);
        let (key, keycode) = xkb_key(&chord.code)?;
        Some(Self { mods, key, keycode })
    }

    /// The modifier names in mask order, as Hyprland writes them.
    pub fn mod_names(&self) -> Vec<&'static str> {
        mod_names(self.mods)
    }

    /// "SUPER + ALT + F11", the Lua config's form.
    pub fn lua(&self) -> String {
        let mut parts = self.mod_names();
        parts.push(self.key);
        parts.join(" + ")
    }

    /// "SUPER ALT,F11", the legacy `bind` form.
    pub fn legacy(&self) -> String {
        format!("{},{}", self.mod_names().join(" "), self.key)
    }

    /// Whether `bind` fires on the same keys.
    pub fn matches(&self, bind: &Bind) -> bool {
        if bind.mouse || !bind.submap.is_empty() || bind.modmask != self.mods {
            return false;
        }
        if bind.key.is_empty() {
            bind.keycode != 0 && bind.keycode == self.keycode
        } else {
            same_key(&bind.key, self.key)
        }
    }
}

pub fn mod_names(mask: u32) -> Vec<&'static str> {
    [
        (SUPER, "SUPER"),
        (CTRL, "CTRL"),
        (ALT, "ALT"),
        (SHIFT, "SHIFT"),
    ]
    .into_iter()
    .filter(|(bit, _)| mask & bit != 0)
    .map(|(_, name)| name)
    .collect()
}

/// xkb matches key names without regard to case, and some keys have two.
fn same_key(a: &str, b: &str) -> bool {
    let canon = |k: &str| match k.to_ascii_lowercase().as_str() {
        "page_up" => "prior".to_string(),
        "page_down" => "next".to_string(),
        "enter" => "return".to_string(),
        other => other.to_string(),
    };
    canon(a) == canon(b)
}

/// The xkb keysym name and the evdev keycode (plus 8, as xkb counts) for a
/// W3C key code from [`Chord`].
pub fn xkb_key(code: &str) -> Option<(&'static str, u32)> {
    const LETTERS: [(&str, u32); 26] = [
        ("A", 38),
        ("B", 56),
        ("C", 54),
        ("D", 40),
        ("E", 26),
        ("F", 41),
        ("G", 42),
        ("H", 43),
        ("I", 31),
        ("J", 44),
        ("K", 45),
        ("L", 46),
        ("M", 58),
        ("N", 57),
        ("O", 32),
        ("P", 33),
        ("Q", 24),
        ("R", 27),
        ("S", 39),
        ("T", 28),
        ("U", 30),
        ("V", 55),
        ("W", 25),
        ("X", 53),
        ("Y", 29),
        ("Z", 52),
    ];
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    const F: [&str; 20] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14",
        "F15", "F16", "F17", "F18", "F19", "F20",
    ];
    let named = match code {
        "ArrowLeft" => ("Left", 113),
        "ArrowUp" => ("Up", 111),
        "ArrowRight" => ("Right", 114),
        "ArrowDown" => ("Down", 116),
        "Space" => ("space", 65),
        "Enter" => ("Return", 36),
        "Tab" => ("Tab", 23),
        "Escape" => ("Escape", 9),
        "Backspace" => ("BackSpace", 22),
        "Delete" => ("Delete", 119),
        "Home" => ("Home", 110),
        "End" => ("End", 115),
        "PageUp" => ("Prior", 112),
        "PageDown" => ("Next", 117),
        "Minus" => ("minus", 20),
        "Equal" => ("equal", 21),
        "BracketLeft" => ("bracketleft", 34),
        "BracketRight" => ("bracketright", 35),
        "Backslash" => ("backslash", 51),
        "Semicolon" => ("semicolon", 47),
        "Quote" => ("apostrophe", 48),
        "Comma" => ("comma", 59),
        "Period" => ("period", 60),
        "Slash" => ("slash", 61),
        "Backquote" => ("grave", 49),
        _ => {
            if let Some(letter) = code.strip_prefix("Key") {
                return LETTERS.iter().find(|(l, _)| *l == letter).copied();
            }
            if let Some(digit) = code.strip_prefix("Digit") {
                let n: usize = digit.parse().ok().filter(|n| *n < 10)?;
                // 1 to 9 are keycodes 10 to 18, and 0 is 19.
                let keycode = if n == 0 { 19 } else { 9 + n as u32 };
                return Some((DIGITS[n], keycode));
            }
            let n: usize = code.strip_prefix('F')?.parse().ok()?;
            if !(1..=20).contains(&n) {
                return None;
            }
            let keycode = match n {
                1..=10 => 66 + n as u32,
                11 => 95,
                12 => 96,
                _ => 178 + n as u32, // F13 is 191
            };
            return Some((F[n - 1], keycode));
        }
    };
    Some(named)
}

/// Commands, rendered for a dialect. Window ids come in as numbers, so
/// selectors never carry text from a plugin.
pub mod cmd {
    use super::{Dialect, Keys, address, lua_quote};
    use crate::window::Rect;

    pub fn focus(d: Dialect, id: u64) -> String {
        let w = address(id);
        match d {
            Dialect::Lua => format!(
                "eval hl.dispatch(hl.dsp.focus({{ window = {} }}))",
                lua_quote(&w)
            ),
            Dialect::Legacy => format!("dispatch focuswindow {w}"),
        }
    }

    /// Closes the window, as its close button does.
    pub fn close(d: Dialect, id: u64) -> String {
        let w = address(id);
        match d {
            Dialect::Lua => format!(
                "eval hl.dispatch(hl.dsp.window.close({{ window = {} }}))",
                lua_quote(&w)
            ),
            Dialect::Legacy => format!("dispatch closewindow {w}"),
        }
    }

    /// Moves the window to `workspace` without following it there, or
    /// with `follow`, then focuses it.
    pub fn move_to(d: Dialect, id: u64, workspace: &str, follow: bool) -> String {
        let w = address(id);
        match d {
            Dialect::Lua => {
                let mut lua = format!(
                    "eval local w = {} hl.dispatch(hl.dsp.window.move({{ workspace = {}, follow = {follow}, window = w }}))",
                    lua_quote(&w),
                    lua_quote(workspace)
                );
                if follow {
                    lua.push_str(" hl.dispatch(hl.dsp.focus({ window = w }))");
                }
                lua
            }
            Dialect::Legacy if follow => format!(
                "[[BATCH]]dispatch movetoworkspace {workspace},{w};dispatch focuswindow {w}"
            ),
            Dialect::Legacy => format!("dispatch movetoworkspacesilent {workspace},{w}"),
        }
    }

    /// Floats a tiled window first, then sets its size and its position.
    pub fn set_frame(d: Dialect, id: u64, float: bool, f: Rect) -> String {
        let w = address(id);
        match d {
            Dialect::Lua => {
                let mut lua = format!("eval local w = {} ", lua_quote(&w));
                if float {
                    lua.push_str(
                        "hl.dispatch(hl.dsp.window.float({ action = \"on\", window = w })) ",
                    );
                }
                lua.push_str(&format!(
                    "hl.dispatch(hl.dsp.window.resize({{ x = {}, y = {}, window = w }})) \
                     hl.dispatch(hl.dsp.window.move({{ x = {}, y = {}, window = w }}))",
                    f.width, f.height, f.x, f.y
                ));
                lua
            }
            Dialect::Legacy => {
                let mut parts = Vec::new();
                if float {
                    parts.push(format!("dispatch setfloating {w}"));
                }
                parts.push(format!(
                    "dispatch resizewindowpixel exact {} {},{w}",
                    f.width, f.height
                ));
                parts.push(format!(
                    "dispatch movewindowpixel exact {} {},{w}",
                    f.x, f.y
                ));
                format!("[[BATCH]]{}", parts.join(";"))
            }
        }
    }

    /// Binds `keys` to emit `custom>>bricolo-hotkey:<token>` on the event socket.
    pub fn bind(d: Dialect, keys: &Keys, token: &str, description: &str) -> String {
        let event = format!("bricolo-hotkey:{token}");
        match d {
            Dialect::Lua => format!(
                "eval hl.bind({}, hl.dsp.event({}), {{ description = {} }})",
                lua_quote(&keys.lua()),
                lua_quote(&event),
                lua_quote(description)
            ),
            Dialect::Legacy => format!("keyword bind {},event,{event}", keys.legacy()),
        }
    }

    pub fn unbind(d: Dialect, keys: &Keys) -> String {
        match d {
            Dialect::Lua => format!("eval hl.unbind({})", lua_quote(&keys.lua())),
            Dialect::Legacy => format!("keyword unbind {}", keys.legacy()),
        }
    }

    /// Presses and releases `keys` in the focused window. The Lua form
    /// sends the down and up states 50 ms apart, as Omarchy does, since
    /// `send_shortcut` can leave a key stuck.
    pub fn keystroke(d: Dialect, keys: &Keys) -> String {
        let mods = keys.mod_names().join(" ");
        match d {
            Dialect::Lua => format!(
                "eval local m, k = {}, {} \
                 hl.dispatch(hl.dsp.send_key_state({{ mods = m, key = k, state = \"down\" }})) \
                 hl.timer(function() \
                 hl.dispatch(hl.dsp.send_key_state({{ mods = m, key = k, state = \"up\" }})) \
                 end, {{ timeout = 50, type = \"oneshot\" }})",
                lua_quote(&mods),
                lua_quote(keys.key)
            ),
            Dialect::Legacy => format!("dispatch sendshortcut {mods},{},", keys.key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(chord: &str) -> Keys {
        Keys::from_chord(&Chord::parse(chord).unwrap()).unwrap()
    }

    #[test]
    fn finds_the_socket_folder() {
        let runtime = tempfile::tempdir().unwrap();
        let sig = Path::new("abc_1_2");
        assert_eq!(socket_dir(Some(runtime.path()), sig), None);
        let dir = runtime.path().join("hypr").join(sig);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".socket.sock"), "").unwrap();
        assert_eq!(socket_dir(Some(runtime.path()), sig), Some(dir));
    }

    #[test]
    fn reads_ok_replies() {
        assert!(reply_ok("ok"));
        assert!(reply_ok("ok\n\nok\n\nok\n"));
        assert!(!reply_ok(""));
        assert!(!reply_ok("ok\n\nerror: no such window"));
        assert!(!reply_ok("unknown request"));
    }

    #[test]
    fn quotes_lua_strings() {
        assert_eq!(lua_quote("plain"), "\"plain\"");
        assert_eq!(lua_quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(lua_quote("x\u{1}y"), "\"x\\001y\"");
        // A closing quote and a call cannot escape the literal.
        assert_eq!(lua_quote("\") os.exit(\""), "\"\\\") os.exit(\\\"\"");
    }

    #[test]
    fn maps_chords_to_hyprland_keys() {
        let k = keys("cmd+alt+left");
        assert_eq!((k.mods, k.key, k.keycode), (SUPER | ALT, "Left", 113));
        assert_eq!(k.lua(), "SUPER + ALT + Left");
        assert_eq!(k.legacy(), "SUPER ALT,Left");
        assert_eq!(keys("ctrl+shift+q").lua(), "CTRL + SHIFT + Q");
        assert_eq!(keys("f12").lua(), "F12");
        assert_eq!(keys("f12").legacy(), ",F12");
        assert_eq!(keys("cmd+1").keycode, 10);
        assert_eq!(keys("cmd+0").keycode, 19);
        assert_eq!(keys("f1").keycode, 67);
        assert_eq!(keys("f10").keycode, 76);
        assert_eq!(keys("f11").keycode, 95);
        assert_eq!(keys("f13").keycode, 191);
        assert_eq!(keys("cmd+/").key, "slash");
        assert_eq!(keys("pageup").key, "Prior");
    }

    #[test]
    fn matches_binds_by_name_or_keycode() {
        let bind = |modmask, key: &str, keycode| Bind {
            modmask,
            key: key.into(),
            keycode,
            ..Bind::default()
        };
        let k = keys("cmd+1");
        assert!(k.matches(&bind(SUPER, "1", 0)));
        // Omarchy binds workspaces as SUPER + code:10.
        assert!(k.matches(&bind(SUPER, "", 10)));
        assert!(!k.matches(&bind(SUPER | SHIFT, "", 10)));
        assert!(!k.matches(&bind(SUPER, "2", 0)));
        assert!(keys("cmd+q").matches(&bind(SUPER, "q", 0)));
        assert!(keys("enter").matches(&bind(0, "RETURN", 0)));
        assert!(keys("pagedown").matches(&bind(0, "Page_Down", 0)));
        let mut in_submap = bind(SUPER, "1", 0);
        in_submap.submap = "resize".into();
        assert!(!k.matches(&in_submap));
    }

    #[test]
    fn reads_code_binds_from_the_text_listing() {
        let text = "bindd\n\tmodmask: 64\n\tsubmap: \n\tkey: SUPER + code:10\n\tkeycode: 0\n\t\
                    description: Switch to workspace 1\n\tdispatcher: __lua\n\targ: 112\n\n\
                    bindd\n\tmodmask: 64\n\tsubmap: \n\tkey: SUPER + L\n\tkeycode: 0\n\n\
                    bind\n\tmodmask: 65\n\tsubmap: resize\n\tkey: SUPER + SHIFT + code:11\n";
        let binds = code_binds(text);
        assert_eq!(binds.len(), 2);
        assert_eq!((binds[0].modmask, binds[0].keycode), (64, 10));
        assert_eq!(binds[0].description, "Switch to workspace 1");
        assert!(keys("cmd+1").matches(&binds[0]));
        assert_eq!((binds[1].modmask, binds[1].keycode), (65, 11));
        assert_eq!(binds[1].submap, "resize");
        assert!(!keys("cmd+shift+2").matches(&binds[1]));
    }

    #[test]
    fn parses_clients_and_addresses() {
        let json = r#"[{"address":"0x55fe1313a980","mapped":true,"hidden":false,
            "at":[10,26],"size":[430,707],"class":"kitty","title":"~","pid":42,
            "floating":false,"fullscreen":0,"workspace":{"id":1,"name":"1"}},
            {"address":"0x1","mapped":false,"at":[0,0],"size":[0,0],"class":"","title":""}]"#;
        let clients: Vec<Client> = serde_json::from_str(json).unwrap();
        let info = clients[0].info().unwrap();
        assert_eq!(info.id, 0x55fe1313a980);
        assert_eq!(info.app, "kitty");
        assert_eq!(
            info.frame,
            Rect {
                x: 10,
                y: 26,
                width: 430,
                height: 707
            }
        );
        assert!(clients[0].is_app_window());
        assert!(!clients[1].is_app_window());
        assert!(!clients[0].is_fullscreen());
        assert_eq!(clients[0].workspace.id, 1);
        assert!(!clients[0].is_minimized());
        let parked: Client = serde_json::from_str(
            r#"{"address":"0x3","workspace":{"id":-98,"name":"special:bricolo-minimized"}}"#,
        )
        .unwrap();
        assert!(parked.is_minimized());
        assert_eq!(address(0x55fe1313a980), "address:0x55fe1313a980");
        assert_eq!(parse_address("0xzz"), None);
        let old: Client = serde_json::from_str(r#"{"address":"0x2","fullscreen":true}"#).unwrap();
        assert!(old.is_fullscreen());
    }

    #[test]
    fn scales_monitors_into_layout_coordinates() {
        let m = Monitor {
            id: 1,
            x: 3440,
            y: 0,
            width: 2880,
            height: 1800,
            scale: 2.0,
            transform: 0,
            reserved: [0, 26, 0, 0],
            focused: false,
            disabled: false,
        };
        let s = m.screen();
        assert_eq!(
            s.frame,
            Rect {
                x: 3440,
                y: 0,
                width: 1440,
                height: 900
            }
        );
        assert_eq!(
            s.visible_frame,
            Rect {
                x: 3440,
                y: 26,
                width: 1440,
                height: 874
            }
        );
        let turned = Monitor {
            transform: 1,
            ..m.clone()
        }
        .screen();
        assert_eq!((turned.frame.width, turned.frame.height), (900, 1440));
        // With no monitor focused, the first is main.
        let list = screens(&[m.clone(), Monitor { id: 2, ..m }]);
        assert!(list[0].main && !list[1].main);
    }

    #[test]
    fn renders_lua_commands() {
        let id = 0xabc;
        assert_eq!(
            cmd::focus(Dialect::Lua, id),
            "eval hl.dispatch(hl.dsp.focus({ window = \"address:0xabc\" }))"
        );
        let frame = Rect {
            x: 100,
            y: 150,
            width: 700,
            height: 400,
        };
        let set = cmd::set_frame(Dialect::Lua, id, true, frame);
        assert!(set.starts_with("eval local w = \"address:0xabc\" "));
        assert!(set.contains("float({ action = \"on\", window = w })"));
        assert!(set.contains("resize({ x = 700, y = 400, window = w })"));
        assert!(set.ends_with("move({ x = 100, y = 150, window = w }))"));
        assert!(!cmd::set_frame(Dialect::Lua, id, false, frame).contains("float"));
        let k = keys("cmd+alt+f11");
        assert_eq!(
            cmd::bind(Dialect::Lua, &k, "7-1", "bricolo[7]: p/\"x\""),
            "eval hl.bind(\"SUPER + ALT + F11\", hl.dsp.event(\"bricolo-hotkey:7-1\"), \
             { description = \"bricolo[7]: p/\\\"x\\\"\" })"
        );
        assert_eq!(
            cmd::unbind(Dialect::Lua, &k),
            "eval hl.unbind(\"SUPER + ALT + F11\")"
        );
        assert_eq!(
            cmd::close(Dialect::Lua, id),
            "eval hl.dispatch(hl.dsp.window.close({ window = \"address:0xabc\" }))"
        );
        assert_eq!(
            cmd::move_to(Dialect::Lua, id, MINIMIZED, false),
            "eval local w = \"address:0xabc\" hl.dispatch(hl.dsp.window.move(\
             { workspace = \"special:bricolo-minimized\", follow = false, window = w }))"
        );
        assert!(
            cmd::move_to(Dialect::Lua, id, "3", true).ends_with(
                "follow = true, window = w })) hl.dispatch(hl.dsp.focus({ window = w }))"
            )
        );
        assert!(cmd::keystroke(Dialect::Lua, &keys("ctrl+v")).starts_with(
            "eval local m, k = \"CTRL\", \"V\" hl.dispatch(hl.dsp.send_key_state({ mods = m"
        ));
    }

    #[test]
    fn renders_legacy_commands() {
        let id = 0xabc;
        assert_eq!(
            cmd::focus(Dialect::Legacy, id),
            "dispatch focuswindow address:0xabc"
        );
        let frame = Rect {
            x: -5,
            y: 10,
            width: 800,
            height: 600,
        };
        assert_eq!(
            cmd::close(Dialect::Legacy, id),
            "dispatch closewindow address:0xabc"
        );
        assert_eq!(
            cmd::move_to(Dialect::Legacy, id, MINIMIZED, false),
            "dispatch movetoworkspacesilent special:bricolo-minimized,address:0xabc"
        );
        assert_eq!(
            cmd::move_to(Dialect::Legacy, id, "3", true),
            "[[BATCH]]dispatch movetoworkspace 3,address:0xabc;dispatch focuswindow address:0xabc"
        );
        assert_eq!(
            cmd::set_frame(Dialect::Legacy, id, true, frame),
            "[[BATCH]]dispatch setfloating address:0xabc;\
             dispatch resizewindowpixel exact 800 600,address:0xabc;\
             dispatch movewindowpixel exact -5 10,address:0xabc"
        );
        let k = keys("cmd+shift+h");
        assert_eq!(
            cmd::bind(Dialect::Legacy, &k, "7-1", "ignored"),
            "keyword bind SUPER SHIFT,H,event,bricolo-hotkey:7-1"
        );
        assert_eq!(
            cmd::unbind(Dialect::Legacy, &k),
            "keyword unbind SUPER SHIFT,H"
        );
        assert_eq!(
            cmd::keystroke(Dialect::Legacy, &keys("ctrl+v")),
            "dispatch sendshortcut CTRL,V,"
        );
    }
}
