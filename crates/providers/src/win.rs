//! Portable logic for the Windows backends in `windows/`: key tables, the
//! modifier masks, the window filter, the frame compensation, monitor ids,
//! app names, the app search for `launch` and the app-event diff. Nothing
//! here touches Win32, so the tests run on every system; the module is
//! compiled on Windows and in tests.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::accessibility::Kind;
use crate::app::{AppInfo, check_app};
use crate::error::CallError;
use crate::event::AppChange;
use crate::hotkey::Modifier;
use crate::window::Rect;

// Keys

/// The set-1 scan code of a key on a US keyboard, for a W3C code from
/// `Chord`, and whether it carries the E0 (extended) prefix. Sending scan
/// codes presses physical positions, whatever the layout.
pub fn scan_code(code: &str) -> Option<(u16, bool)> {
    let plain = |sc| Some((sc, false));
    let extended = |sc| Some((sc, true));
    match code {
        "Escape" => plain(0x01),
        "Minus" => plain(0x0c),
        "Equal" => plain(0x0d),
        "Backspace" => plain(0x0e),
        "Tab" => plain(0x0f),
        "BracketLeft" => plain(0x1a),
        "BracketRight" => plain(0x1b),
        "Enter" => plain(0x1c),
        "Semicolon" => plain(0x27),
        "Quote" => plain(0x28),
        "Backquote" => plain(0x29),
        "Backslash" => plain(0x2b),
        "Comma" => plain(0x33),
        "Period" => plain(0x34),
        "Slash" => plain(0x35),
        "Space" => plain(0x39),
        "Home" => extended(0x47),
        "ArrowUp" => extended(0x48),
        "PageUp" => extended(0x49),
        "ArrowLeft" => extended(0x4b),
        "ArrowRight" => extended(0x4d),
        "End" => extended(0x4f),
        "ArrowDown" => extended(0x50),
        "PageDown" => extended(0x51),
        "Delete" => extended(0x53),
        _ => {
            if let Some(letter) = code.strip_prefix("Key") {
                // The letters in the order of their scan codes, row by row.
                const ROWS: [(&str, u16); 3] =
                    [("QWERTYUIOP", 0x10), ("ASDFGHJKL", 0x1e), ("ZXCVBNM", 0x2c)];
                let [c] = letter.as_bytes() else { return None };
                return ROWS.iter().find_map(|(row, first)| {
                    let i = row.bytes().position(|b| b == *c)?;
                    Some((first + i as u16, false))
                });
            }
            if let Some(digit) = code.strip_prefix("Digit") {
                let [c] = digit.as_bytes() else { return None };
                return match c {
                    b'0' => plain(0x0b),
                    b'1'..=b'9' => plain(0x02 + u16::from(c - b'1')),
                    _ => None,
                };
            }
            let n: u16 = code.strip_prefix('F')?.parse().ok()?;
            match n {
                1..=10 => plain(0x3b + n - 1),
                11 => plain(0x57),
                12 => plain(0x58),
                13..=20 => plain(0x64 + n - 13),
                _ => None,
            }
        }
    }
}

/// One window's picture for [`compose`]: its frame on the desktop and its
/// pixels as `GetDIBits` gives them, BGRX rows top to bottom.
pub struct Layer<'a> {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub bgrx: &'a [u8],
}

/// A `width` by `height` RGBA picture on white, with each layer drawn over
/// the ones before it and clipped to the picture.
pub fn compose(width: u32, height: u32, layers: &[Layer]) -> Vec<u8> {
    let (w, h) = (width as i64, height as i64);
    let mut out = vec![255u8; (w * h * 4) as usize];
    for l in layers {
        if l.bgrx.len() < l.width as usize * l.height as usize * 4 {
            continue;
        }
        for row in 0..l.height as i64 {
            let y = l.y as i64 + row;
            if !(0..h).contains(&y) {
                continue;
            }
            for col in 0..l.width as i64 {
                let x = l.x as i64 + col;
                if !(0..w).contains(&x) {
                    continue;
                }
                let src = ((row * l.width as i64 + col) * 4) as usize;
                let dst = ((y * w + x) * 4) as usize;
                out[dst] = l.bgrx[src + 2];
                out[dst + 1] = l.bgrx[src + 1];
                out[dst + 2] = l.bgrx[src];
                out[dst + 3] = 255;
            }
        }
    }
    out
}

/// The first of `frames` (topmost first, as `EnumDesktopWindows` lists
/// them) that holds the point, as `(x, y, width, height)`.
pub fn window_at(frames: &[(i32, i32, i32, i32)], x: i32, y: i32) -> Option<usize> {
    frames
        .iter()
        .position(|&(fx, fy, fw, fh)| x >= fx && y >= fy && x < fx + fw && y < fy + fh)
}

/// The `LPARAM` of a mouse message: x in the low word, y in the high one.
pub fn point_lparam(x: i32, y: i32) -> isize {
    ((y as u16 as u32) << 16 | (x as u16 as u32)) as i32 as isize
}

/// The UTF-16 units `WM_CHAR` takes for `text`; a line break is Enter (CR).
pub fn char_units(text: &str) -> Vec<u16> {
    text.replace("\r\n", "\r")
        .replace('\n', "\r")
        .encode_utf16()
        .collect()
}

/// The virtual-key code of a key on a US layout. Keys that type no
/// character (arrows, F-keys, Enter) have the same code on every layout;
/// for the others see [`is_character_key`].
pub fn virtual_key(code: &str) -> Option<u16> {
    let vk = match code {
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Space" => 0x20,
        "Enter" => 0x0d,
        "Tab" => 0x09,
        "Escape" => 0x1b,
        "Backspace" => 0x08,
        "Delete" => 0x2e,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "Minus" => 0xbd,
        "Equal" => 0xbb,
        "BracketLeft" => 0xdb,
        "BracketRight" => 0xdd,
        "Backslash" => 0xdc,
        "Semicolon" => 0xba,
        "Quote" => 0xde,
        "Comma" => 0xbc,
        "Period" => 0xbe,
        "Slash" => 0xbf,
        "Backquote" => 0xc0,
        _ => {
            if let Some(letter) = code.strip_prefix("Key") {
                let [c] = letter.as_bytes() else { return None };
                return c.is_ascii_uppercase().then_some(u16::from(*c));
            }
            if let Some(digit) = code.strip_prefix("Digit") {
                let [c] = digit.as_bytes() else { return None };
                return c.is_ascii_digit().then_some(u16::from(*c));
            }
            let n: u16 = code.strip_prefix('F')?.parse().ok()?;
            // VK_F1 is 0x70 and the rest follow in order.
            return (1..=20).contains(&n).then_some(0x70 + n - 1);
        }
    };
    Some(vk)
}

/// Whether the key types a character, so the layout decides its
/// virtual-key code: letters, digits and punctuation.
pub fn is_character_key(code: &str) -> bool {
    code.starts_with("Key")
        || code.starts_with("Digit")
        || matches!(
            code,
            "Minus"
                | "Equal"
                | "BracketLeft"
                | "BracketRight"
                | "Backslash"
                | "Semicolon"
                | "Quote"
                | "Comma"
                | "Period"
                | "Slash"
                | "Backquote"
        )
}

/// `RegisterHotKey`'s modifier flags.
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;
/// Holding the chord down sends one press, not a stream of them.
pub const MOD_NOREPEAT: u32 = 0x4000;

/// The `RegisterHotKey` flags for a chord's modifiers: `cmd` is the
/// Windows key. Always with [`MOD_NOREPEAT`].
pub fn hotkey_modifiers(mods: &[Modifier]) -> u32 {
    mods.iter()
        .map(|m| match m {
            Modifier::Alt => MOD_ALT,
            Modifier::Ctrl => MOD_CONTROL,
            Modifier::Shift => MOD_SHIFT,
            Modifier::Cmd => MOD_WIN,
        })
        .fold(MOD_NOREPEAT, |mask, bit| mask | bit)
}

/// The scan code of the left key for each modifier: Ctrl, Shift, Alt and
/// the Windows key for `cmd`.
pub fn modifier_scan(m: Modifier) -> (u16, bool) {
    match m {
        Modifier::Ctrl => (0x1d, false),
        Modifier::Shift => (0x2a, false),
        Modifier::Alt => (0x38, false),
        Modifier::Cmd => (0x5b, true),
    }
}

/// The virtual keys of the modifier keys, left and right, that
/// `keyboard` releases before it presses anything: Shift, Ctrl, Alt, Win.
pub const MODIFIER_KEYS: [u16; 8] = [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0x5b, 0x5c];

/// Whether releasing `vk` alone does something: the Windows key opens the
/// Start menu and Alt the window's menu bar, unless another key went down
/// while they were held.
pub fn needs_mask(vk: u16) -> bool {
    matches!(vk, 0xa4 | 0xa5 | 0x5b | 0x5c)
}

/// An unassigned virtual key, pressed between a held Win or Alt and its
/// release so that the release does nothing (AutoHotkey's "menu mask").
pub const MASK_KEY: u16 = 0xe8;

// Mouse

/// The most wheel notches one call sends, so a typo cannot spin for long.
pub const MAX_NOTCHES: u32 = 1000;
/// One notch of the wheel.
pub const WHEEL_DELTA: i32 = 120;

/// The wheel data of one notch and how many notches make a scroll of
/// `lines`. The vertical wheel counts away from the user as positive, so
/// `invert` is true for `dy` (positive scrolls down); the horizontal one
/// counts right as positive, as `dx` does.
pub fn wheel(lines: i32, invert: bool) -> Option<(i32, u32)> {
    if lines == 0 {
        return None;
    }
    let toward_positive = (lines > 0) != invert;
    let data = if toward_positive {
        WHEEL_DELTA
    } else {
        -WHEEL_DELTA
    };
    Some((data, lines.unsigned_abs().min(MAX_NOTCHES)))
}

// Windows

/// What the window list needs to know about a top-level window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowTraits {
    pub visible: bool,
    /// Hidden by DWM: a suspended Store app, or a window on another
    /// virtual desktop.
    pub cloaked: bool,
    pub tool_window: bool,
    pub app_window: bool,
    pub no_activate: bool,
    /// Has an owner window: a dialog or a palette.
    pub owned: bool,
    pub class: String,
    pub title: String,
}

/// Window classes of the shell itself: the desktop and the taskbars.
const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// Whether a window is one the taskbar and Alt+Tab show: visible, not
/// cloaked, with a title, and either marked `WS_EX_APPWINDOW` or neither a
/// tool window, an unactivatable one nor owned by another.
pub fn is_app_window(t: &WindowTraits) -> bool {
    if !t.visible || t.cloaked || t.title.is_empty() || SHELL_CLASSES.contains(&t.class.as_str()) {
        return false;
    }
    t.app_window || !(t.tool_window || t.no_activate || t.owned)
}

/// A Win32 `RECT`: edges, right and bottom exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Edges {
    pub fn to_rect(self) -> Rect {
        Rect {
            x: self.left,
            y: self.top,
            width: self.right.saturating_sub(self.left).max(0) as u32,
            height: self.bottom.saturating_sub(self.top).max(0) as u32,
        }
    }
}

/// The `SetWindowPos` position and size that put a window's visible frame
/// at `frame`. `outer` is its `GetWindowRect`, which on Windows 10 and
/// later includes invisible resize borders; `visible` is DWM's extended
/// frame bounds. The difference on each side is added back.
pub fn compensate(frame: Rect, outer: Edges, visible: Edges) -> (i32, i32, i32, i32) {
    let left = (visible.left - outer.left).max(0);
    let top = (visible.top - outer.top).max(0);
    let right = (outer.right - visible.right).max(0);
    let bottom = (outer.bottom - visible.bottom).max(0);
    let width = i32::try_from(frame.width).unwrap_or(i32::MAX);
    let height = i32::try_from(frame.height).unwrap_or(i32::MAX);
    (
        frame.x.saturating_sub(left),
        frame.y.saturating_sub(top),
        width.saturating_add(left + right),
        height.saturating_add(top + bottom),
    )
}

/// A monitor's id: the number in its GDI device name (`\\.\DISPLAY2` is
/// 2), else `index + 1`.
pub fn monitor_id(device: &str, index: usize) -> u32 {
    let digits: String = device
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| u32::try_from(index + 1).unwrap_or(u32::MAX))
}

/// An app's name: its program's file name without `.exe` (any case),
/// from a full path with `\` or `/` separators.
pub fn app_name(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    strip_exe(file).to_string()
}

fn strip_exe(name: &str) -> &str {
    let n = name.len();
    if n > 4 && name.is_char_boundary(n - 4) && name[n - 4..].eq_ignore_ascii_case(".exe") {
        &name[..n - 4]
    } else {
        name
    }
}

/// Whether the app named `name` (an exe stem) is the one asked for as
/// `wanted`, with or without `.exe`, in any case.
pub fn same_app(name: &str, wanted: &str) -> bool {
    !name.is_empty() && name.eq_ignore_ascii_case(strip_exe(wanted.trim()))
}

// Launching

/// `app` trimmed, if it names an app: no path, drive, wildcard or option.
pub fn check_app_name(app: &str) -> Result<&str, CallError> {
    let app = check_app(app)?;
    if app.contains(['\\', ':', '*', '?', '"', '<', '>', '|']) {
        return Err(CallError::Failed(format!(
            "{app:?} is not an app name or bundle id"
        )));
    }
    Ok(app)
}

/// The Start Menu's `Programs` folders: the user's (under `%APPDATA%`),
/// then everyone's (under `%ProgramData%`).
pub fn start_menu_dirs(appdata: Option<&OsStr>, program_data: Option<&OsStr>) -> Vec<PathBuf> {
    [appdata, program_data]
        .into_iter()
        .flatten()
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(r"Microsoft\Windows\Start Menu\Programs"))
        .collect()
}

/// The first shortcut (`.lnk`) named `name`, in any case, under `dirs` and
/// four levels of folders below them, the first folder first.
pub fn find_shortcut(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: u8) -> Option<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        paths.sort();
        let (folders, files): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.is_dir());
        let hit = files.into_iter().find(|p| {
            p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
                && p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name))
        });
        if hit.is_some() || depth >= 4 {
            return hit;
        }
        folders.iter().find_map(|f| walk(f, name, depth + 1))
    }
    dirs.iter().find_map(|d| walk(d, name, 0))
}

/// `name.exe` (or `name` when it ends in `.exe`) in the first folder of
/// the `PATH` list `path` that has it.
pub fn find_on_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    let file = exe_file(name);
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
}

fn exe_file(name: &str) -> String {
    format!("{}.exe", strip_exe(name))
}

/// The registry key under HKCU and HKLM that registers `name` in App
/// Paths, the list the Run dialog reads.
pub fn app_paths_key(name: &str) -> String {
    format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}",
        exe_file(name)
    )
}

/// A registry path value without the quotes some installers put around it.
pub fn unquote(value: &str) -> &str {
    let v = value.trim();
    v.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(v)
}

// App events

/// What changed since the last look at the apps with windows. `known`
/// holds the apps seen so far, by process id; `current` is what has app
/// windows now. An app that is new is `Launched`. One whose windows are
/// gone counts as `Terminated` only once its process has exited, as
/// `alive` tells; until then it stays known, so closing its last window
/// and opening another is no launch.
pub fn diff_apps(
    known: &mut BTreeMap<i32, AppInfo>,
    current: &[AppInfo],
    alive: impl Fn(i32) -> bool,
) -> Vec<(AppChange, AppInfo)> {
    let mut changes = Vec::new();
    for app in current {
        if let std::collections::btree_map::Entry::Vacant(slot) = known.entry(app.pid) {
            slot.insert(app.clone());
            changes.push((AppChange::Launched, app.clone()));
        }
    }
    let gone: Vec<i32> = known
        .keys()
        .copied()
        .filter(|pid| !current.iter().any(|a| a.pid == *pid) && !alive(*pid))
        .collect();
    for pid in gone {
        if let Some(app) = known.remove(&pid) {
            changes.push((AppChange::Terminated, app));
        }
    }
    changes
}

// UI Automation (`windows/accessibility.rs`)

/// Maps a UI Automation `ControlType` id (`UIA_*ControlTypeId`, 50000 and
/// up) onto the engine's [`Kind`].
pub fn uia_kind(control_type: i32) -> Kind {
    match control_type {
        50000 | 50031 => Kind::Button, // Button, SplitButton
        50002 => Kind::CheckBox,
        50003 => Kind::ComboBox,
        50004 => Kind::TextField,           // Edit
        50005 => Kind::Link,                // Hyperlink
        50006 => Kind::Image,               // Image
        50007 | 50024 | 50029 => Kind::Row, // ListItem, TreeItem, DataItem
        50008 | 50023 => Kind::List,        // List, Tree
        50009 => Kind::Menu,
        50010 => Kind::MenuBar,
        50011 => Kind::MenuItem,
        50013 => Kind::RadioButton,
        50014 => Kind::ScrollBar,
        50015 | 50016 => Kind::Slider, // Slider, Spinner
        50018 => Kind::TabGroup,
        50019 => Kind::Tab,
        50020 => Kind::StaticText,            // Text
        50021 | 50040 => Kind::Toolbar,       // ToolBar, AppBar
        50026 | 50033 | 50034 => Kind::Group, // Group, Pane, Header
        50028 | 50036 => Kind::Table,         // DataGrid, Table
        50030 => Kind::TextArea,              // Document
        50032 => Kind::Window,
        50035 => Kind::Cell, // HeaderItem
        // ProgressBar, StatusBar, ToolTip, Custom, Thumb, TitleBar,
        // Separator, SemanticZoom, Calendar and ids not known yet.
        _ => Kind::Other,
    }
}

/// The English name of a control type, for when the provider gives no
/// localized one.
pub fn uia_role_name(control_type: i32) -> &'static str {
    match control_type {
        50000 => "button",
        50001 => "calendar",
        50002 => "check box",
        50003 => "combo box",
        50004 => "edit",
        50005 => "link",
        50006 => "image",
        50007 => "list item",
        50008 => "list",
        50009 => "menu",
        50010 => "menu bar",
        50011 => "menu item",
        50012 => "progress bar",
        50013 => "radio button",
        50014 => "scroll bar",
        50015 => "slider",
        50016 => "spinner",
        50017 => "status bar",
        50018 => "tab",
        50019 => "tab item",
        50020 => "text",
        50021 => "tool bar",
        50022 => "tool tip",
        50023 => "tree",
        50024 => "tree item",
        50026 => "group",
        50027 => "thumb",
        50028 => "data grid",
        50029 => "data item",
        50030 => "document",
        50031 => "split button",
        50032 => "window",
        50033 => "pane",
        50034 => "header",
        50035 => "header item",
        50036 => "table",
        50037 => "title bar",
        50038 => "separator",
        50039 => "semantic zoom",
        50040 => "app bar",
        _ => "element",
    }
}

/// The role a line shows: the localized control type, lowercased, else
/// the English name.
pub fn uia_role(localized: &str, control_type: i32) -> String {
    let t = localized.trim();
    if t.is_empty() {
        uia_role_name(control_type).to_string()
    } else {
        t.to_lowercase()
    }
}

/// Controls whose patterns are not worth the calls: they only show text
/// or draw.
pub fn uia_skips_patterns(control_type: i32) -> bool {
    matches!(control_type, 50006 | 50020 | 50038 | 50027)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpandState {
    Collapsed,
    Expanded,
    Partial,
}

/// From `ExpandCollapseState`: 0 collapsed, 1 expanded, 2 partly, 3 leaf.
pub fn expand_state(state: i32) -> Option<ExpandState> {
    match state {
        0 => Some(ExpandState::Collapsed),
        1 => Some(ExpandState::Expanded),
        2 => Some(ExpandState::Partial),
        _ => None,
    }
}

/// The patterns an element supports, as far as actions care.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Patterns {
    pub invoke: bool,
    pub toggle: bool,
    pub select: bool,
    pub scroll_item: bool,
    pub expand: Option<ExpandState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiaAction {
    Invoke,
    Toggle,
    Select,
    Expand,
    Collapse,
    ScrollIntoView,
}

impl UiaAction {
    pub fn name(self) -> &'static str {
        match self {
            UiaAction::Invoke => "invoke",
            UiaAction::Toggle => "toggle",
            UiaAction::Select => "select",
            UiaAction::Expand => "expand",
            UiaAction::Collapse => "collapse",
            // The engine's line rules already hide this one under the
            // name macOS gives it ("scroll to visible"), because every
            // list item has it.
            UiaAction::ScrollIntoView => "scroll to visible",
        }
    }
}

/// The action names `perform` accepts for these patterns, as lines list
/// them.
pub fn uia_actions(p: &Patterns) -> Vec<String> {
    let mut out = Vec::new();
    if p.invoke {
        out.push(UiaAction::Invoke);
    }
    if p.toggle {
        out.push(UiaAction::Toggle);
    }
    if p.select {
        out.push(UiaAction::Select);
    }
    match p.expand {
        Some(ExpandState::Collapsed) => out.push(UiaAction::Expand),
        Some(ExpandState::Expanded) => out.push(UiaAction::Collapse),
        Some(ExpandState::Partial) => {
            out.push(UiaAction::Expand);
            out.push(UiaAction::Collapse);
        }
        None => {}
    }
    if p.scroll_item {
        out.push(UiaAction::ScrollIntoView);
    }
    out.into_iter().map(|a| a.name().to_string()).collect()
}

/// Reads an action name back: "scroll into view", "Scroll-To-Visible" and
/// "scrolltovisible" are one action.
pub fn parse_uia_action(name: &str) -> Option<UiaAction> {
    let key: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match key.as_str() {
        "invoke" => Some(UiaAction::Invoke),
        "toggle" => Some(UiaAction::Toggle),
        "select" => Some(UiaAction::Select),
        "expand" => Some(UiaAction::Expand),
        "collapse" => Some(UiaAction::Collapse),
        "scrolltovisible" | "scrollintoview" => Some(UiaAction::ScrollIntoView),
        _ => None,
    }
}

/// What a click does to an element: invoke, else toggle, else select,
/// else open or close it. `scroll_item` never counts.
pub fn press_action(p: &Patterns) -> Option<UiaAction> {
    if p.invoke {
        Some(UiaAction::Invoke)
    } else if p.toggle {
        Some(UiaAction::Toggle)
    } else if p.select {
        Some(UiaAction::Select)
    } else {
        match p.expand {
            Some(ExpandState::Collapsed | ExpandState::Partial) => Some(UiaAction::Expand),
            Some(ExpandState::Expanded) => Some(UiaAction::Collapse),
            None => None,
        }
    }
}

/// A stable number for a UIA runtime id (FNV-1a over its parts).
pub fn runtime_id_hash(parts: &[i32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// An element's frame: none when it has no area (offscreen elements
/// report an empty rectangle).
pub fn ax_frame(e: Edges) -> Option<Rect> {
    let r = e.to_rect();
    (r.width > 0 && r.height > 0).then_some(r)
}

/// `current` with the UTF-16 range `start..end` replaced by `text`. A
/// range outside the text appends instead. UIA counts text in UTF-16 units.
pub fn splice_utf16(current: &str, range: Option<(usize, usize)>, text: &str) -> String {
    let units: Vec<u16> = current.encode_utf16().collect();
    let (start, end) = match range {
        Some((s, e)) if s <= e && e <= units.len() => (s, e),
        _ => (units.len(), units.len()),
    };
    let mut out = String::from_utf16_lossy(&units[..start]);
    out.push_str(text);
    out.push_str(&String::from_utf16_lossy(&units[end..]));
    out
}

/// Line breaks as an edit control wants them: `\r\n` (Windows edits) or
/// `\n` alone (RichEdit counts one character).
pub fn edit_newlines(text: &str, crlf: bool) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    if crlf {
        unified.replace('\n', "\r\n")
    } else {
        unified
    }
}

/// Whether a window class is a user32 edit or a RichEdit, which answer
/// `WM_GETTEXT` and `EM_REPLACESEL` from other processes. WinForms
/// superclasses them as `WindowsForms10.EDIT.app.0.…`.
pub fn is_edit_class(class: &str) -> bool {
    let lower = class.to_lowercase();
    let base = lower
        .strip_prefix("windowsforms10.")
        .unwrap_or(&lower)
        .split('.')
        .next()
        .unwrap_or("");
    base == "edit" || base.starts_with("richedit")
}

/// A number as a value shows it: `50` for 50.0.
pub fn fmt_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiaFault {
    /// The element or its process is gone.
    Gone,
    NotEnabled,
    NotClickable,
    NotSupported,
    Denied,
    Other,
}

/// Sorts the HRESULT of a failed UI Automation call.
pub fn classify_hresult(code: i32) -> UiaFault {
    match code as u32 {
        // UIA_E_ELEMENTNOTAVAILABLE, RPC_E_DISCONNECTED, RPC_E_SERVER_DIED,
        // RPC_E_SERVER_DIED_DNE, RPC_S_SERVER_UNAVAILABLE, RPC_S_CALL_FAILED
        0x8004_0201 | 0x8001_0108 | 0x8001_0007 | 0x8001_0012 | 0x8007_06ba | 0x8007_06be => {
            UiaFault::Gone
        }
        0x8004_0200 => UiaFault::NotEnabled, // UIA_E_ELEMENTNOTENABLED
        0x8004_0202 => UiaFault::NotClickable, // UIA_E_NOCLICKABLEPOINT
        0x8004_0204 => UiaFault::NotSupported, // UIA_E_NOTSUPPORTED
        0x8007_0005 => UiaFault::Denied,     // E_ACCESSDENIED
        _ => UiaFault::Other,
    }
}

// Hidden desktops (`windows/virtual_screen.rs`)

/// The name of a hidden desktop. Desktop names are unique per window
/// station, so the node's process id keeps two nodes of one user apart.
pub fn hidden_desktop_name(pid: u32, n: u32) -> String {
    format!("bricolo-virtual-{pid}-{n}")
}

/// One argument as `CommandLineToArgvW` reads it back: quoted when it has
/// a space, tab, quote or is empty; backslashes before a quote doubled.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// The command line `CreateProcessW` takes for a program and arguments.
pub fn command_line(program: &str, args: &[String]) -> String {
    let mut line = quote_arg(program);
    for a in args {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_window_pictures_with_clipping_and_order() {
        // A red 2x1 window at (1, 0) under a blue 1x1 window at (2, 0), on a
        // 3x2 canvas; one more window lies outside it.
        let red = [0, 0, 255, 0, 0, 0, 255, 0];
        let blue = [255, 0, 0, 0];
        let out = compose(
            3,
            2,
            &[
                Layer {
                    x: 1,
                    y: 0,
                    width: 2,
                    height: 1,
                    bgrx: &red,
                },
                Layer {
                    x: 2,
                    y: 0,
                    width: 1,
                    height: 1,
                    bgrx: &blue,
                },
                Layer {
                    x: -5,
                    y: 9,
                    width: 1,
                    height: 1,
                    bgrx: &blue,
                },
            ],
        );
        assert_eq!(&out[0..4], [255, 255, 255, 255]);
        assert_eq!(&out[4..8], [255, 0, 0, 255]);
        assert_eq!(&out[8..12], [0, 0, 255, 255]);
        assert_eq!(&out[12..16], [255, 255, 255, 255]);
    }

    #[test]
    fn finds_the_topmost_window_and_packs_points() {
        let frames = [(10, 10, 100, 100), (0, 0, 500, 500)];
        assert_eq!(window_at(&frames, 50, 50), Some(0));
        assert_eq!(window_at(&frames, 5, 5), Some(1));
        assert_eq!(window_at(&frames, 600, 5), None);
        assert_eq!(point_lparam(3, 2), 0x0002_0003);
        assert_eq!(point_lparam(-1, 0) & 0xffff, 0xffff);
        assert_eq!(char_units("a\r\nb\n"), [97, 13, 98, 13]);
    }

    #[test]
    fn text_and_image_controls_skip_patterns() {
        assert!(uia_skips_patterns(50020)); // text
        assert!(uia_skips_patterns(50006)); // image
        assert!(!uia_skips_patterns(50000)); // button
    }
    use crate::hotkey::Chord;

    fn code(chord: &str) -> String {
        Chord::parse(chord).unwrap().code
    }

    /// Every key `Chord` accepts, as chords.
    fn every_key() -> Vec<String> {
        let mut keys: Vec<String> = "abcdefghijklmnopqrstuvwxyz0123456789-=[]\\;',./`"
            .chars()
            .map(|c| format!("ctrl+{c}"))
            .collect();
        for k in [
            "left",
            "right",
            "up",
            "down",
            "space",
            "enter",
            "tab",
            "escape",
            "delete",
            "forwarddelete",
            "home",
            "end",
            "pageup",
            "pagedown",
        ] {
            keys.push(k.to_string());
        }
        keys.extend((1..=20).map(|n| format!("f{n}")));
        keys
    }

    #[test]
    fn every_chord_key_has_a_scan_code_and_a_virtual_key() {
        for chord in every_key() {
            let c = code(&chord);
            assert!(scan_code(&c).is_some(), "{chord}");
            assert!(virtual_key(&c).is_some(), "{chord}");
        }
        assert_eq!(scan_code("KeyAB"), None);
        assert_eq!(scan_code("F21"), None);
        assert_eq!(virtual_key("F21"), None);
        assert_eq!(virtual_key("Fn"), None);
    }

    #[test]
    fn scan_codes_follow_the_us_rows() {
        assert_eq!(scan_code(&code("q")), Some((0x10, false)));
        assert_eq!(scan_code(&code("p")), Some((0x19, false)));
        assert_eq!(scan_code(&code("a")), Some((0x1e, false)));
        assert_eq!(scan_code(&code("l")), Some((0x26, false)));
        assert_eq!(scan_code(&code("z")), Some((0x2c, false)));
        assert_eq!(scan_code(&code("m")), Some((0x32, false)));
        assert_eq!(scan_code(&code("1")), Some((0x02, false)));
        assert_eq!(scan_code(&code("9")), Some((0x0a, false)));
        assert_eq!(scan_code(&code("0")), Some((0x0b, false)));
        assert_eq!(scan_code(&code("f10")), Some((0x44, false)));
        assert_eq!(scan_code(&code("f12")), Some((0x58, false)));
        assert_eq!(scan_code(&code("f13")), Some((0x64, false)));
        assert_eq!(scan_code(&code("f20")), Some((0x6b, false)));
        assert_eq!(scan_code(&code("left")), Some((0x4b, true)));
        assert_eq!(scan_code(&code("forwarddelete")), Some((0x53, true)));
        assert_eq!(scan_code(&code("backspace")), Some((0x0e, false)));
        // Every letter has its own scan code.
        let mut seen: Vec<u16> = ('a'..='z')
            .map(|c| scan_code(&code(&c.to_string())).unwrap().0)
            .collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 26);
    }

    #[test]
    fn virtual_keys_match_winuser() {
        assert_eq!(virtual_key(&code("a")), Some(0x41));
        assert_eq!(virtual_key(&code("7")), Some(0x37));
        assert_eq!(virtual_key(&code("f1")), Some(0x70));
        assert_eq!(virtual_key(&code("f20")), Some(0x83));
        assert_eq!(virtual_key(&code("left")), Some(0x25));
        assert_eq!(virtual_key(&code("enter")), Some(0x0d));
        assert_eq!(virtual_key(&code("/")), Some(0xbf));
        assert!(is_character_key(&code("a")));
        assert!(is_character_key(&code("`")));
        assert!(!is_character_key(&code("f5")));
        assert!(!is_character_key(&code("space")));
    }

    #[test]
    fn modifiers_map_to_hotkey_flags() {
        let mask = |c: &str| hotkey_modifiers(&Chord::parse(c).unwrap().mods);
        assert_eq!(mask("cmd+alt+left"), MOD_WIN | MOD_ALT | MOD_NOREPEAT);
        assert_eq!(mask("ctrl+shift+k"), MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT);
        assert_eq!(mask("f1"), MOD_NOREPEAT);
        assert_eq!(modifier_scan(Modifier::Cmd), (0x5b, true));
        assert!(needs_mask(0x5b) && needs_mask(0xa4) && !needs_mask(0xa0));
        // Win and Alt, left and right, need the mask key; it is none of them.
        let masked: Vec<u16> = MODIFIER_KEYS
            .into_iter()
            .filter(|vk| needs_mask(*vk))
            .collect();
        assert_eq!(masked, [0xa4, 0xa5, 0x5b, 0x5c]);
        assert!(!MODIFIER_KEYS.contains(&MASK_KEY));
    }

    #[test]
    fn wheel_signs_follow_the_wit() {
        // Positive dy scrolls down: the wheel turns toward the user.
        assert_eq!(wheel(3, true), Some((-120, 3)));
        assert_eq!(wheel(-2, true), Some((120, 2)));
        // Positive dx scrolls right.
        assert_eq!(wheel(4, false), Some((120, 4)));
        assert_eq!(wheel(-1, false), Some((-120, 1)));
        assert_eq!(wheel(0, true), None);
        assert_eq!(wheel(i32::MIN, false), Some((-120, MAX_NOTCHES)));
    }

    fn window(f: impl FnOnce(&mut WindowTraits)) -> WindowTraits {
        let mut t = WindowTraits {
            visible: true,
            class: "Notepad".into(),
            title: "Untitled - Notepad".into(),
            ..WindowTraits::default()
        };
        f(&mut t);
        t
    }

    #[test]
    fn the_window_list_keeps_what_alt_tab_shows() {
        assert!(is_app_window(&window(|_| {})));
        assert!(!is_app_window(&window(|t| t.visible = false)));
        assert!(!is_app_window(&window(|t| t.cloaked = true)));
        assert!(!is_app_window(&window(|t| t.title.clear())));
        assert!(!is_app_window(&window(|t| t.tool_window = true)));
        assert!(!is_app_window(&window(|t| t.no_activate = true)));
        assert!(!is_app_window(&window(|t| t.owned = true)));
        assert!(!is_app_window(&window(|t| t.class = "Progman".into())));
        // WS_EX_APPWINDOW puts an owned or tool window back.
        assert!(is_app_window(&window(|t| {
            t.owned = true;
            t.tool_window = true;
            t.app_window = true;
        })));
        // But not a hidden or cloaked one.
        assert!(!is_app_window(&window(|t| {
            t.app_window = true;
            t.cloaked = true;
        })));
    }

    #[test]
    fn frames_add_back_the_invisible_borders() {
        // Windows 10: 7 px invisible borders left, right and bottom.
        let outer = Edges {
            left: 93,
            top: 100,
            right: 907,
            bottom: 707,
        };
        let visible = Edges {
            left: 100,
            top: 100,
            right: 900,
            bottom: 700,
        };
        assert_eq!(
            visible.to_rect(),
            Rect {
                x: 100,
                y: 100,
                width: 800,
                height: 600
            }
        );
        let frame = Rect {
            x: 0,
            y: 0,
            width: 960,
            height: 1040,
        };
        assert_eq!(compensate(frame, outer, visible), (-7, 0, 974, 1047));
        // No borders: the frame as it is.
        assert_eq!(compensate(frame, visible, visible), (0, 0, 960, 1040));
        // An outer rect inside the visible one never shrinks the frame.
        assert_eq!(compensate(frame, visible, outer), (0, 0, 960, 1040));
        assert_eq!(Edges::default().to_rect(), Rect::default());
        let backwards = Edges {
            left: 10,
            right: 5,
            ..Edges::default()
        };
        assert_eq!(backwards.to_rect().width, 0);
    }

    #[test]
    fn monitors_take_the_number_of_their_device() {
        assert_eq!(monitor_id(r"\\.\DISPLAY1", 0), 1);
        assert_eq!(monitor_id(r"\\.\DISPLAY12", 0), 12);
        assert_eq!(monitor_id("", 2), 3);
        assert_eq!(monitor_id(r"\\.\DISPLAY", 0), 1);
    }

    #[test]
    fn app_names_are_exe_stems() {
        assert_eq!(app_name(r"C:\Windows\System32\notepad.exe"), "notepad");
        assert_eq!(app_name(r"C:\Program Files\App\Code.EXE"), "Code");
        assert_eq!(app_name("C:/x/tool"), "tool");
        assert_eq!(app_name(".exe"), ".exe");
        assert!(same_app("notepad", "Notepad"));
        assert!(same_app("notepad", "notepad.exe"));
        assert!(!same_app("notepad", "note"));
        assert!(!same_app("", ""));
    }

    #[test]
    fn app_names_refuse_paths() {
        assert_eq!(check_app_name(" notepad ").unwrap(), "notepad");
        assert!(check_app_name("Visual Studio Code").is_ok());
        assert!(check_app_name(r"C:\Windows\notepad.exe").is_err());
        assert!(check_app_name("ms-settings:display").is_err());
        assert!(check_app_name("*").is_err());
        assert!(check_app_name("a/b").is_err());
        assert!(check_app_name("-x").is_err());
    }

    #[test]
    fn launch_looks_in_the_start_menu_path_and_app_paths() {
        let appdata = tempfile::tempdir().unwrap();
        let common = tempfile::tempdir().unwrap();
        let dirs = start_menu_dirs(
            Some(appdata.path().as_os_str()),
            Some(common.path().as_os_str()),
        );
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].starts_with(appdata.path()));
        assert!(dirs[0].ends_with(r"Microsoft\Windows\Start Menu\Programs"));
        assert_eq!(
            start_menu_dirs(None, Some(OsStr::new(""))),
            [] as [PathBuf; 0]
        );

        let user = appdata.path().join("Programs");
        let all = common.path().join("Programs");
        std::fs::create_dir_all(all.join("Accessories")).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(all.join("Accessories").join("Notepad.lnk"), "").unwrap();
        std::fs::write(all.join("Visual Studio Code.LNK"), "").unwrap();
        std::fs::write(user.join("Visual Studio Code.lnk"), "").unwrap();
        std::fs::write(user.join("readme.txt"), "").unwrap();
        let dirs = [user.clone(), all.clone()];
        assert_eq!(
            find_shortcut("notepad", &dirs),
            Some(all.join("Accessories").join("Notepad.lnk"))
        );
        // The user's folder comes first.
        assert_eq!(
            find_shortcut("visual studio code", &dirs),
            Some(user.join("Visual Studio Code.lnk"))
        );
        assert_eq!(find_shortcut("readme", &dirs), None);
        assert_eq!(find_shortcut("nothing", &dirs), None);

        let bin = tempfile::tempdir().unwrap();
        std::fs::write(bin.path().join("tool.exe"), "").unwrap();
        let path = std::env::join_paths([user.as_path(), bin.path()]).unwrap();
        assert_eq!(
            find_on_path("tool", &path),
            Some(bin.path().join("tool.exe"))
        );
        assert_eq!(
            find_on_path("tool.exe", &path),
            Some(bin.path().join("tool.exe"))
        );
        assert_eq!(find_on_path("other", &path), None);

        assert_eq!(
            app_paths_key("chrome"),
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\chrome.exe"
        );
        assert!(app_paths_key("chrome.exe").ends_with(r"\chrome.exe"));
        assert_eq!(unquote(r#" "C:\a b\c.exe" "#), r"C:\a b\c.exe");
        assert_eq!(unquote(r"C:\c.exe"), r"C:\c.exe");
    }

    fn app(name: &str, pid: i32) -> AppInfo {
        AppInfo {
            name: name.into(),
            bundle_id: None,
            pid,
        }
    }

    #[test]
    fn app_events_come_from_the_window_owners() {
        let mut known = BTreeMap::new();
        let all_alive = |_| true;
        // The first look reports what runs as launched; the watcher drops
        // that first batch.
        let first = diff_apps(&mut known, &[app("explorer", 1), app("code", 2)], all_alive);
        assert_eq!(first.len(), 2);
        assert!(diff_apps(&mut known, &[app("explorer", 1), app("code", 2)], all_alive).is_empty());

        let launched = diff_apps(
            &mut known,
            &[app("explorer", 1), app("code", 2), app("notepad", 3)],
            all_alive,
        );
        assert_eq!(launched, [(AppChange::Launched, app("notepad", 3))]);

        // Code closes its windows but still runs: nothing yet.
        assert!(
            diff_apps(
                &mut known,
                &[app("explorer", 1), app("notepad", 3)],
                all_alive
            )
            .is_empty()
        );
        // Then it exits.
        let exited = diff_apps(
            &mut known,
            &[app("explorer", 1), app("notepad", 3)],
            |pid| pid != 2,
        );
        assert_eq!(exited, [(AppChange::Terminated, app("code", 2))]);
        assert!(!known.contains_key(&2));
    }

    #[test]
    fn uia_control_types_map_to_kinds() {
        assert_eq!(uia_kind(50000), Kind::Button);
        assert_eq!(uia_kind(50004), Kind::TextField);
        assert_eq!(uia_kind(50030), Kind::TextArea);
        assert_eq!(uia_kind(50010), Kind::MenuBar);
        assert_eq!(uia_kind(50011), Kind::MenuItem);
        assert_eq!(uia_kind(50032), Kind::Window);
        assert_eq!(uia_kind(50033), Kind::Group);
        assert_eq!(uia_kind(50025), Kind::Other);
        assert_eq!(uia_kind(1), Kind::Other);
        assert!(uia_kind(50004).editable() && uia_kind(50030).editable());
    }

    #[test]
    fn uia_roles_prefer_the_localized_name() {
        assert_eq!(uia_role(" Schaltfläche ", 50000), "schaltfläche");
        assert_eq!(uia_role("", 50000), "button");
        assert_eq!(uia_role("", 99999), "element");
    }

    #[test]
    fn uia_actions_follow_the_patterns() {
        let p = Patterns {
            invoke: true,
            expand: Some(ExpandState::Collapsed),
            scroll_item: true,
            ..Patterns::default()
        };
        assert_eq!(uia_actions(&p), ["invoke", "expand", "scroll to visible"]);
        let both = Patterns {
            select: true,
            expand: Some(ExpandState::Partial),
            ..Patterns::default()
        };
        assert_eq!(uia_actions(&both), ["select", "expand", "collapse"]);
        assert!(uia_actions(&Patterns::default()).is_empty());
    }

    #[test]
    fn uia_action_names_read_back() {
        for a in [
            UiaAction::Invoke,
            UiaAction::Toggle,
            UiaAction::Select,
            UiaAction::Expand,
            UiaAction::Collapse,
            UiaAction::ScrollIntoView,
        ] {
            assert_eq!(parse_uia_action(a.name()), Some(a));
        }
        assert_eq!(
            parse_uia_action("Scroll into view"),
            Some(UiaAction::ScrollIntoView)
        );
        assert_eq!(parse_uia_action("INVOKE"), Some(UiaAction::Invoke));
        assert_eq!(parse_uia_action("set value"), None);
    }

    #[test]
    fn a_click_picks_the_first_pattern_that_acts() {
        let mut p = Patterns {
            toggle: true,
            select: true,
            expand: Some(ExpandState::Expanded),
            ..Patterns::default()
        };
        assert_eq!(press_action(&p), Some(UiaAction::Toggle));
        p.invoke = true;
        assert_eq!(press_action(&p), Some(UiaAction::Invoke));
        p.invoke = false;
        p.toggle = false;
        assert_eq!(press_action(&p), Some(UiaAction::Select));
        p.select = false;
        assert_eq!(press_action(&p), Some(UiaAction::Collapse));
        p.expand = Some(ExpandState::Collapsed);
        assert_eq!(press_action(&p), Some(UiaAction::Expand));
        p.expand = None;
        p.scroll_item = true;
        assert_eq!(press_action(&p), None);
    }

    #[test]
    fn expand_states_read_from_uia() {
        assert_eq!(expand_state(0), Some(ExpandState::Collapsed));
        assert_eq!(expand_state(1), Some(ExpandState::Expanded));
        assert_eq!(expand_state(2), Some(ExpandState::Partial));
        assert_eq!(expand_state(3), None);
    }

    #[test]
    fn runtime_ids_hash_to_stable_distinct_numbers() {
        assert_eq!(runtime_id_hash(&[42, 7, 1]), runtime_id_hash(&[42, 7, 1]));
        assert_ne!(runtime_id_hash(&[42, 7, 1]), runtime_id_hash(&[42, 7, 2]));
        assert_ne!(runtime_id_hash(&[1, 2]), runtime_id_hash(&[2, 1]));
    }

    #[test]
    fn elements_without_area_have_no_frame() {
        let edges = |l, t, r, b| Edges {
            left: l,
            top: t,
            right: r,
            bottom: b,
        };
        assert_eq!(ax_frame(edges(0, 0, 0, 0)), None);
        assert_eq!(ax_frame(edges(10, 10, 10, 50)), None);
        assert_eq!(
            ax_frame(edges(-5, 20, 95, 60)),
            Some(Rect {
                x: -5,
                y: 20,
                width: 100,
                height: 40
            })
        );
    }

    #[test]
    fn text_splices_by_utf16_offsets() {
        assert_eq!(
            splice_utf16("hello world", Some((6, 11)), "there"),
            "hello there"
        );
        assert_eq!(splice_utf16("hello", Some((2, 2)), "XY"), "heXYllo");
        // Outside the text: append.
        assert_eq!(splice_utf16("hello", Some((3, 99)), "!"), "hello!");
        assert_eq!(splice_utf16("hello", None, "!"), "hello!");
        // A character outside the BMP counts as two units.
        assert_eq!(splice_utf16("a😀b", Some((3, 4)), "-"), "a😀-");
    }

    #[test]
    fn edit_text_gets_the_line_breaks_the_control_counts() {
        assert_eq!(edit_newlines("a\nb\r\nc\rd", true), "a\r\nb\r\nc\r\nd");
        assert_eq!(edit_newlines("a\r\nb\rc", false), "a\nb\nc");
    }

    #[test]
    fn edit_classes_are_recognised() {
        for yes in [
            "Edit",
            "RichEdit20W",
            "RICHEDIT50W",
            "RichEditD2DPT",
            "WindowsForms10.EDIT.app.0.141b42a_r6_ad1",
            "WindowsForms10.RichEdit20W.app.0.1",
        ] {
            assert!(is_edit_class(yes), "{yes}");
        }
        for no in [
            "Button",
            "ComboBox",
            "EditorFrame",
            "WindowsForms10.BUTTON.app.0.1",
            "",
        ] {
            assert!(!is_edit_class(no), "{no}");
        }
    }

    #[test]
    fn numbers_show_without_a_trailing_zero() {
        assert_eq!(fmt_number(50.0), "50");
        assert_eq!(fmt_number(-3.0), "-3");
        assert_eq!(fmt_number(0.5), "0.5");
    }

    #[test]
    fn uia_failures_are_sorted() {
        assert_eq!(classify_hresult(0x8004_0201_u32 as i32), UiaFault::Gone);
        assert_eq!(classify_hresult(0x8001_0108_u32 as i32), UiaFault::Gone);
        assert_eq!(
            classify_hresult(0x8004_0200_u32 as i32),
            UiaFault::NotEnabled
        );
        assert_eq!(
            classify_hresult(0x8004_0202_u32 as i32),
            UiaFault::NotClickable
        );
        assert_eq!(
            classify_hresult(0x8004_0204_u32 as i32),
            UiaFault::NotSupported
        );
        assert_eq!(classify_hresult(0x8007_0005_u32 as i32), UiaFault::Denied);
        assert_eq!(classify_hresult(0x8000_4005_u32 as i32), UiaFault::Other);
    }

    #[test]
    fn hidden_desktops_are_named_per_node_and_screen() {
        assert_eq!(hidden_desktop_name(4242, 1), "bricolo-virtual-4242-1");
        assert_ne!(hidden_desktop_name(4242, 1), hidden_desktop_name(4243, 1));
        assert_ne!(hidden_desktop_name(4242, 1), hidden_desktop_name(4242, 2));
    }

    #[test]
    fn command_lines_quote_like_the_runtime_reads_them() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        // Backslashes only matter before a quote or the closing quote.
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_arg(r"C:\dir\file"), r"C:\dir\file");
        assert_eq!(
            command_line(r"C:\Program Files\x.exe", &["a b".into(), "c".into()]),
            r#""C:\Program Files\x.exe" "a b" c"#
        );
    }
}
