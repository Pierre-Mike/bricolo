//! `accessibility`: the UI tree of an app's windows, and actions on its
//! elements. The tree reading, the element indexes, the rendering, the
//! verification of actions and the refusals live here, free of any OS
//! code; each system brings an [`AxBackend`]: `AXUIElement` on macOS
//! (`macos/accessibility.rs`), UI Automation on Windows
//! (`windows/accessibility.rs`) and AT-SPI on Linux
//! (`linux/accessibility.rs`). The tests drive a fake desktop
//! ([`fake::FakeDesktop`]).
//!
//! The design follows Orca's computer use (github.com/stablyai/orca,
//! `native/computer-use-*`): one line per element with a leading index,
//! unnamed groups elided, the children of compact controls folded into
//! their line, a node and depth limit with `... N more not shown` markers,
//! indexes that stay put while an element exists, and actions that report
//! whether their effect was read back. The rendering rules in [`render`]
//! are ported from Orca's `SnapshotRendering.swift`:
//!
//! > Copyright (c) 2026 Lovecast Inc. MIT License. Permission is hereby
//! > granted, free of charge, to any person obtaining a copy of this
//! > software and associated documentation files (the "Software"), to deal
//! > in the Software without restriction, including without limitation the
//! > rights to use, copy, modify, merge, publish, distribute, sublicense,
//! > and/or sell copies of the Software, and to permit persons to whom the
//! > Software is furnished to do so, subject to the following conditions:
//! > The above copyright notice and this permission notice shall be
//! > included in all copies or substantial portions of the Software.
//! > THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.
//!
//! `accessibility` never runs on a peer through rules.toml: it acts on
//! this device's screen. The MCP gateway reaches it on each node by name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::caller::Caller;
use crate::error::CallError;
use crate::hotkey::{Chord, Modifier};
use crate::screen::ScreenProvider;
use crate::window::Rect;

/// Elements one read takes at most, as in Orca.
pub const MAX_NODES: usize = 1200;
/// Levels one read descends at most.
pub const MAX_DEPTH: usize = 64;
/// Characters of a value a line shows.
const MAX_VALUE: usize = 400;
/// How long an action waits for the window to change.
const SETTLE: Duration = Duration::from_millis(1000);
/// `wait` without a timeout.
const WAIT_DEFAULT: Duration = Duration::from_secs(10);
const WAIT_MAX: Duration = Duration::from_secs(120);
/// Elements a cache keeps before it starts over.
const CACHE_LIMIT: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxApp {
    pub name: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
    pub frontmost: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxWindow {
    pub index: u32,
    pub title: String,
    pub frame: Option<Rect>,
    pub focused: bool,
    pub minimized: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    pub app: String,
    pub pid: i32,
    pub window: String,
    pub tree: String,
    pub element_count: u32,
    pub truncated: bool,
    pub focused: Option<u32>,
    pub screenshot: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Element {
    pub index: u32,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub frame: Option<Rect>,
    pub enabled: bool,
    pub focused: bool,
    pub actions: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verification {
    Verified,
    Unverified,
    SuspectedNoop,
    Mismatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    pub verification: Verification,
    pub detail: Option<String>,
    pub state: AppState,
}

#[async_trait]
pub trait AccessibilityProvider: Send + Sync {
    async fn list_apps(&self, caller: &Caller) -> Result<Vec<AxApp>, CallError>;
    async fn list_windows(&self, caller: &Caller, app: String) -> Result<Vec<AxWindow>, CallError>;
    async fn snapshot(
        &self,
        caller: &Caller,
        app: String,
        window: Option<u32>,
        query: Option<String>,
        element: Option<u32>,
        screenshot: Option<bool>,
    ) -> Result<AppState, CallError>;
    async fn find(
        &self,
        caller: &Caller,
        app: String,
        text: String,
    ) -> Result<Vec<Element>, CallError>;
    async fn click(
        &self,
        caller: &Caller,
        app: String,
        element: u32,
    ) -> Result<ActionResult, CallError>;
    async fn set_value(
        &self,
        caller: &Caller,
        app: String,
        element: u32,
        value: String,
    ) -> Result<ActionResult, CallError>;
    async fn type_text(
        &self,
        caller: &Caller,
        app: String,
        element: Option<u32>,
        text: String,
    ) -> Result<ActionResult, CallError>;
    async fn press_key(
        &self,
        caller: &Caller,
        app: String,
        key: String,
    ) -> Result<ActionResult, CallError>;
    async fn perform(
        &self,
        caller: &Caller,
        app: String,
        element: u32,
        action: String,
    ) -> Result<ActionResult, CallError>;
    async fn menu(
        &self,
        caller: &Caller,
        app: String,
        path: Vec<String>,
    ) -> Result<ActionResult, CallError>;
    async fn wait(
        &self,
        caller: &Caller,
        app: String,
        text: String,
        timeout_ms: Option<u32>,
    ) -> Result<Vec<Element>, CallError>;
    /// Forgets the element indexes `plugin` holds.
    fn release(&self, _plugin: &str) {}
}

/// No accessibility on this node.
pub struct NoAccessibility;

#[async_trait]
impl AccessibilityProvider for NoAccessibility {
    async fn list_apps(&self, _: &Caller) -> Result<Vec<AxApp>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn list_windows(&self, _: &Caller, _: String) -> Result<Vec<AxWindow>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn snapshot(
        &self,
        _: &Caller,
        _: String,
        _: Option<u32>,
        _: Option<String>,
        _: Option<u32>,
        _: Option<bool>,
    ) -> Result<AppState, CallError> {
        Err(CallError::Unavailable)
    }
    async fn find(&self, _: &Caller, _: String, _: String) -> Result<Vec<Element>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn click(&self, _: &Caller, _: String, _: u32) -> Result<ActionResult, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_value(
        &self,
        _: &Caller,
        _: String,
        _: u32,
        _: String,
    ) -> Result<ActionResult, CallError> {
        Err(CallError::Unavailable)
    }
    async fn type_text(
        &self,
        _: &Caller,
        _: String,
        _: Option<u32>,
        _: String,
    ) -> Result<ActionResult, CallError> {
        Err(CallError::Unavailable)
    }
    async fn press_key(&self, _: &Caller, _: String, _: String) -> Result<ActionResult, CallError> {
        Err(CallError::Unavailable)
    }
    async fn perform(
        &self,
        _: &Caller,
        _: String,
        _: u32,
        _: String,
    ) -> Result<ActionResult, CallError> {
        Err(CallError::Unavailable)
    }
    async fn menu(&self, _: &Caller, _: String, _: Vec<String>) -> Result<ActionResult, CallError> {
        Err(CallError::Unavailable)
    }
    async fn wait(
        &self,
        _: &Caller,
        _: String,
        _: String,
        _: Option<u32>,
    ) -> Result<Vec<Element>, CallError> {
        Err(CallError::Unavailable)
    }
}

/// What kind of control an element is, across systems. Each backend maps
/// its own roles onto these; the rendering rules only look at these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Application,
    Window,
    Dialog,
    Button,
    CheckBox,
    RadioButton,
    ComboBox,
    PopUpButton,
    Disclosure,
    TextField,
    TextArea,
    StaticText,
    Heading,
    Link,
    Image,
    Slider,
    Menu,
    MenuBar,
    MenuBarItem,
    MenuItem,
    List,
    Row,
    Cell,
    Table,
    Tab,
    TabGroup,
    ScrollArea,
    ScrollBar,
    Toolbar,
    WebArea,
    Group,
    Other,
}

impl Kind {
    /// Holds text the user edits.
    pub fn editable(self) -> bool {
        matches!(self, Kind::TextField | Kind::TextArea | Kind::ComboBox)
    }
}

/// One element as a backend reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfo {
    pub kind: Kind,
    /// The role as the line shows it: lowercase words, like `button` or
    /// `text field`; the system's own description when it has one.
    pub role: String,
    /// The title.
    pub name: Option<String>,
    /// The accessibility description (macOS), help text or label.
    pub description: Option<String>,
    pub value: Option<String>,
    pub placeholder: Option<String>,
    pub url: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    pub selected: bool,
    /// For disclosures, outline rows and menus: open or closed.
    pub expanded: Option<bool>,
    /// In screen points, top-left origin.
    pub frame: Option<Rect>,
    /// Action names as lines show them: `press`, `increment`, `show menu`.
    pub actions: Vec<String>,
    /// Equal for the same element across reads, as long as it exists.
    pub identity: u64,
}

impl NodeInfo {
    pub fn new(kind: Kind, role: impl Into<String>, identity: u64) -> Self {
        Self {
            kind,
            role: role.into(),
            name: None,
            description: None,
            value: None,
            placeholder: None,
            url: None,
            enabled: true,
            focused: false,
            selected: false,
            expanded: None,
            frame: None,
            actions: Vec::new(),
            identity,
        }
    }
}

/// A window as a backend lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowMeta {
    pub title: String,
    pub frame: Option<Rect>,
    pub focused: bool,
    pub minimized: bool,
}

/// One system's accessibility API. Calls block; the engine runs them on
/// Tokio's blocking pool. Errors that mean "the element is gone" should be
/// `Failed` with text starting `element_not_found`.
pub trait AxBackend: Send + Sync + 'static {
    type Node: Clone + Send + Sync + 'static;

    /// Apps with at least one window.
    fn apps(&self) -> Result<Vec<AxApp>, CallError>;
    fn windows(&self, pid: i32) -> Result<Vec<(Self::Node, WindowMeta)>, CallError>;
    fn info(&self, node: &Self::Node) -> Result<NodeInfo, CallError>;
    fn children(&self, node: &Self::Node) -> Result<Vec<Self::Node>, CallError>;
    /// The element with keyboard focus in the app.
    fn focused(&self, pid: i32) -> Option<Self::Node>;
    /// The app's menu bar, if it has one in its accessibility tree.
    fn menu_bar(&self, pid: i32) -> Option<Self::Node>;
    /// The element's default action: press, invoke, click or select.
    fn press(&self, node: &Self::Node) -> Result<(), CallError>;
    /// One of the names in [`NodeInfo::actions`].
    fn perform(&self, node: &Self::Node, action: &str) -> Result<(), CallError>;
    fn set_value(&self, node: &Self::Node, value: &str) -> Result<(), CallError>;
    /// Inserts at the caret, replacing the selection.
    fn insert_text(&self, node: &Self::Node, text: &str) -> Result<(), CallError>;
    /// Presses the chord in the app. Backends that can only reach the
    /// focused window refuse when the app is not in front.
    fn press_key(&self, pid: i32, chord: &Chord) -> Result<(), CallError>;
    /// Opens a menu or submenu so its items can be picked. Systems whose
    /// menus are in the tree while closed do nothing.
    fn expand(&self, _node: &Self::Node) -> Result<(), CallError> {
        Ok(())
    }
}

/// The index of each element an app's last reads met, for one caller.
struct AppCache<N> {
    nodes: HashMap<u32, N>,
    by_identity: HashMap<u64, u32>,
    next: u32,
}

impl<N> Default for AppCache<N> {
    fn default() -> Self {
        Self {
            nodes: HashMap::new(),
            by_identity: HashMap::new(),
            next: 1,
        }
    }
}

impl<N: Clone> AppCache<N> {
    fn index(&mut self, identity: u64, node: &N) -> u32 {
        if let Some(&i) = self.by_identity.get(&identity) {
            self.nodes.insert(i, node.clone());
            return i;
        }
        let i = self.next;
        self.next += 1;
        self.by_identity.insert(identity, i);
        self.nodes.insert(i, node.clone());
        i
    }
}

/// The accessibility engine over one backend.
pub struct Accessibility<B: AxBackend> {
    inner: Arc<Inner<B>>,
}

/// Each caller's element indexes per app, keyed by caller and pid.
type Caches<N> = Mutex<HashMap<(String, i32), AppCache<N>>>;

struct Inner<B: AxBackend> {
    backend: B,
    caches: Caches<B::Node>,
    screen: Option<Arc<dyn ScreenProvider>>,
    /// The quit, close and log-out chords of this system.
    blocked: &'static [&'static str],
}

/// One rendered line, before `query` filters.
#[derive(Clone, Debug)]
struct Line {
    depth: usize,
    text: String,
    /// The line of the nearest shown ancestor.
    parent: Option<usize>,
    index: Option<u32>,
}

/// One read of a window.
struct Read<N> {
    app: AxApp,
    window: WindowMeta,
    lines: Vec<Line>,
    /// Every element read, by index, with its line.
    elements: Vec<(u32, NodeInfo, N)>,
    truncated: bool,
    focused: Option<u32>,
}

impl<N> Read<N> {
    fn tree(&self, query: Option<&str>) -> String {
        render_lines(&self.lines, query)
    }

    fn state(&self, query: Option<&str>) -> AppState {
        AppState {
            app: self.app.name.clone(),
            pid: self.app.pid,
            window: self.window.title.clone(),
            tree: self.tree(query),
            element_count: self.elements.len() as u32,
            truncated: self.truncated,
            focused: self.focused,
            screenshot: None,
        }
    }

    fn matching(&self, text: &str) -> Vec<Element> {
        let needle = text.to_lowercase();
        let lines: HashMap<u32, &str> = self
            .lines
            .iter()
            .filter_map(|l| Some((l.index?, l.text.as_str())))
            .collect();
        self.elements
            .iter()
            .filter(|(i, info, _)| {
                lines
                    .get(i)
                    .map(|l| l.to_lowercase().contains(&needle))
                    .unwrap_or(false)
                    || info.role.to_lowercase().contains(&needle)
                    || info
                        .name
                        .as_deref()
                        .is_some_and(|n| n.to_lowercase().contains(&needle))
                    || info
                        .value
                        .as_deref()
                        .is_some_and(|v| v.to_lowercase().contains(&needle))
            })
            .map(|(i, info, _)| element(*i, info))
            .collect()
    }
}

fn element(index: u32, info: &NodeInfo) -> Element {
    Element {
        index,
        role: info.role.clone(),
        name: render::display_name(info),
        value: clean(info.value.as_deref()),
        frame: info.frame,
        enabled: info.enabled,
        focused: info.focused,
        actions: info.actions.clone(),
    }
}

fn not_found(app: &str) -> CallError {
    CallError::Failed(format!(
        "app_not_found: no app with a window matches {app:?}; call list-apps and use its bundle id"
    ))
}

fn stale(app: &str, index: u32) -> CallError {
    CallError::Failed(format!(
        "element_not_found: element {index} is not in the last snapshot of {app}; take a new snapshot"
    ))
}

/// Picks the app `selector` names: `pid:<n>`, a bundle id, or a name
/// (exact, then the one name that contains it), any case.
pub fn pick_app<'a>(apps: &'a [AxApp], selector: &str) -> Option<&'a AxApp> {
    let s = selector.trim();
    if let Some(pid) = s.strip_prefix("pid:") {
        let pid: i32 = pid.trim().parse().ok()?;
        return apps.iter().find(|a| a.pid == pid);
    }
    let lower = s.to_lowercase();
    if let Some(a) = apps.iter().find(|a| {
        a.bundle_id
            .as_deref()
            .is_some_and(|b| b.eq_ignore_ascii_case(s))
    }) {
        return Some(a);
    }
    if let Some(a) = apps.iter().find(|a| a.name.to_lowercase() == lower) {
        return Some(a);
    }
    let mut partial = apps
        .iter()
        .filter(|a| a.name.to_lowercase().contains(&lower));
    match (partial.next(), partial.next()) {
        (Some(a), None) => Some(a),
        _ => None,
    }
}

/// The chords that quit or close an app, log out, lock the screen,
/// force-quit or end the session, per system, in [`Chord`]'s text.
pub const BLOCKED_MACOS: &[&str] = &[
    "cmd+KeyQ",
    "alt+cmd+KeyQ",
    "shift+cmd+KeyQ",
    "alt+shift+cmd+KeyQ",
    "ctrl+cmd+KeyQ",
    "alt+cmd+Escape",
    "ctrl+cmd+Power",
];
pub const BLOCKED_WINDOWS: &[&str] = &["alt+F4", "ctrl+alt+Delete", "cmd+KeyL"];
pub const BLOCKED_LINUX: &[&str] = &["alt+F4", "ctrl+KeyQ", "ctrl+alt+Delete", "cmd+KeyL"];

/// This system's list.
pub fn blocked_here() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        BLOCKED_MACOS
    } else if cfg!(target_os = "windows") {
        BLOCKED_WINDOWS
    } else {
        BLOCKED_LINUX
    }
}

/// Parses a key or chord, with `cmdorctrl` meaning cmd on macOS and ctrl
/// elsewhere, and refuses the ones in `blocked`.
pub fn parse_key(text: &str, blocked: &[&str]) -> Result<Chord, CallError> {
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let lower = text.trim().to_lowercase().replace("cmdorctrl", primary);
    let chord =
        Chord::parse(&lower).map_err(|e| CallError::Failed(format!("invalid_argument: {e}")))?;
    let shown = chord.to_string();
    if blocked.contains(&shown.as_str()) {
        return Err(CallError::Failed(format!(
            "system_shortcut_blocked: {text} would quit or close the app, log out, lock or end the \
             session; use the app's own button or menu item instead"
        )));
    }
    Ok(chord)
}

impl<B: AxBackend> Accessibility<B> {
    pub fn new(backend: B, screen: Option<Arc<dyn ScreenProvider>>) -> Self {
        Self::with_blocked(backend, screen, blocked_here())
    }

    pub fn with_blocked(
        backend: B,
        screen: Option<Arc<dyn ScreenProvider>>,
        blocked: &'static [&'static str],
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend,
                caches: Mutex::new(HashMap::new()),
                screen,
                blocked,
            }),
        }
    }

    /// Runs `f` on the blocking pool.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Inner<B>) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || f(&inner))
            .await
            .map_err(CallError::failed)?
    }

    /// Runs one element action, then reads the window again and judges.
    async fn act(
        &self,
        caller: &Caller,
        app: String,
        action: Act,
    ) -> Result<ActionResult, CallError> {
        let plugin = caller.plugin.clone();
        self.blocking(move |inner| inner.act(&plugin, &app, action))
            .await
    }
}

/// What an action does.
enum Act {
    Click(u32),
    SetValue(u32, String),
    Type(Option<u32>, String),
    Key(String),
    Perform(u32, String),
    Menu(Vec<String>),
}

impl<B: AxBackend> Inner<B> {
    fn app(&self, selector: &str) -> Result<AxApp, CallError> {
        let apps = self.backend.apps()?;
        pick_app(&apps, selector)
            .cloned()
            .ok_or_else(|| not_found(selector))
    }

    fn window(&self, app: &AxApp, index: Option<u32>) -> Result<(B::Node, WindowMeta), CallError> {
        let mut windows = self.backend.windows(app.pid)?;
        if windows.is_empty() {
            return Err(CallError::Failed(format!(
                "window_not_found: {} has no window it shows; ask the user to open one",
                app.name
            )));
        }
        let at = match index {
            Some(i) => {
                let i = i as usize;
                if i >= windows.len() {
                    return Err(CallError::Failed(format!(
                        "window_not_found: {} has {} windows; call list-windows",
                        app.name,
                        windows.len()
                    )));
                }
                i
            }
            None => windows.iter().position(|(_, w)| w.focused).unwrap_or(0),
        };
        Ok(windows.swap_remove(at))
    }

    fn cached(&self, plugin: &str, app: &AxApp, index: u32) -> Result<B::Node, CallError> {
        let caches = self.caches.lock().unwrap();
        caches
            .get(&(plugin.to_string(), app.pid))
            .and_then(|c| c.nodes.get(&index))
            .cloned()
            .ok_or_else(|| stale(&app.name, index))
    }

    /// Reads the window (or one element under it) and indexes what it
    /// meets.
    fn read(
        &self,
        plugin: &str,
        app: &AxApp,
        window: Option<u32>,
        element: Option<u32>,
    ) -> Result<Read<B::Node>, CallError> {
        let (win, meta) = self.window(app, window)?;
        let root = match element {
            Some(i) => self.cached(plugin, app, i)?,
            None => win,
        };
        let mut caches = self.caches.lock().unwrap();
        let cache = caches.entry((plugin.to_string(), app.pid)).or_default();
        if cache.nodes.len() > CACHE_LIMIT {
            *cache = AppCache::default();
        }
        let mut walk = Walk {
            backend: &self.backend,
            cache,
            lines: Vec::new(),
            elements: Vec::new(),
            truncated: false,
        };
        walk.visit(&root, 0, None)?;
        let Walk {
            lines,
            elements,
            truncated,
            ..
        } = walk;
        let focused = self.backend.focused(app.pid).and_then(|f| {
            let info = self.backend.info(&f).ok()?;
            elements
                .iter()
                .find(|(_, i, _)| i.identity == info.identity)
                .map(|(i, _, _)| *i)
        });
        Ok(Read {
            app: app.clone(),
            window: meta,
            lines,
            elements,
            truncated,
            focused,
        })
    }

    fn windows_signature(&self, pid: i32) -> Vec<String> {
        self.backend
            .windows(pid)
            .map(|ws| ws.into_iter().map(|(_, w)| w.title).collect())
            .unwrap_or_default()
    }

    /// Waits up to [`SETTLE`] for the tree or the window list to differ
    /// from `before`; returns the last read and whether it changed.
    fn settle(
        &self,
        plugin: &str,
        app: &AxApp,
        before: &str,
        windows: &[String],
    ) -> Result<(AppState, bool), CallError> {
        let start = Instant::now();
        loop {
            let read = self.read(plugin, app, None, None);
            let changed = match &read {
                Ok(r) => r.tree(None) != before || self.windows_signature(app.pid) != windows,
                // The window went away: that is a change.
                Err(_) => true,
            };
            if changed || start.elapsed() >= SETTLE {
                return match read {
                    Ok(r) => Ok((r.state(None), changed)),
                    // The action closed the window, or quit the app: a
                    // result, not a failure.
                    Err(_) => Ok((
                        AppState {
                            app: app.name.clone(),
                            pid: app.pid,
                            tree: "The window closed.\n".into(),
                            ..AppState::default()
                        },
                        true,
                    )),
                };
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn act(&self, plugin: &str, selector: &str, action: Act) -> Result<ActionResult, CallError> {
        let app = self.app(selector)?;
        // The index must come from a read this caller made: look it up
        // before the read below indexes the window again.
        let target = match &action {
            Act::Click(i) | Act::SetValue(i, _) | Act::Perform(i, _) | Act::Type(Some(i), _) => {
                Some(self.cached(plugin, &app, *i)?)
            }
            _ => None,
        };
        let before = self.read(plugin, &app, None, None)?;
        let before_tree = before.tree(None);
        let windows = self.windows_signature(app.pid);
        let enabled = |node: &B::Node, index: u32| -> Result<NodeInfo, CallError> {
            let info = self.backend.info(node)?;
            if !info.enabled {
                return Err(CallError::Failed(format!(
                    "element_not_enabled: element {index} is disabled; do what enables it first"
                )));
            }
            Ok(info)
        };
        // What to read back once the action ran: the node and the text
        // its value should hold or contain.
        let mut readback: Option<(B::Node, String, bool)> = None;
        let mut keys = false;
        match action {
            Act::Click(i) => {
                let node = target.expect("looked up");
                enabled(&node, i)?;
                self.backend.press(&node)?;
            }
            Act::SetValue(i, value) => {
                let node = target.expect("looked up");
                enabled(&node, i)?;
                self.backend.set_value(&node, &value)?;
                readback = Some((node, value, true));
            }
            Act::Type(i, text) => {
                crate::keyboard::check_text(&text)?;
                let node = match i {
                    Some(i) => {
                        let node = target.clone().expect("looked up");
                        enabled(&node, i)?;
                        node
                    }
                    None => self.backend.focused(app.pid).ok_or_else(|| {
                        CallError::Failed(
                            "no_focus: no element has keyboard focus in the app; pass an element"
                                .into(),
                        )
                    })?,
                };
                let info = self.backend.info(&node)?;
                if !info.kind.editable() {
                    return Err(CallError::Failed(format!(
                        "value_not_settable: the {} does not take text",
                        info.role
                    )));
                }
                self.backend.insert_text(&node, &text)?;
                readback = Some((node, text, false));
            }
            Act::Key(key) => {
                let chord = parse_key(&key, self.blocked)?;
                self.backend.press_key(app.pid, &chord)?;
                keys = true;
            }
            Act::Perform(i, action) => {
                let node = target.expect("looked up");
                let info = enabled(&node, i)?;
                let Some(name) = info
                    .actions
                    .iter()
                    .find(|a| a.eq_ignore_ascii_case(action.trim()))
                else {
                    return Err(CallError::Failed(format!(
                        "action_not_supported: element {i} has the actions [{}]",
                        info.actions.join(", ")
                    )));
                };
                self.backend.perform(&node, name)?;
            }
            Act::Menu(path) => self.pick_menu(&app, &path)?,
        }
        if let Some((node, expected, exact)) = readback {
            let got = self.backend.info(&node).ok().and_then(|i| i.value);
            let (r, _) = self.settle(plugin, &app, &before_tree, &windows)?;
            let ok = match &got {
                Some(v) if exact => v == &expected || same_number(v, &expected),
                Some(v) => v.contains(&expected),
                None => false,
            };
            let (verification, detail) = if ok {
                (
                    Verification::Verified,
                    "the value was read back".to_string(),
                )
            } else if got.is_none() {
                (
                    Verification::Unverified,
                    "the element has no value to read back".to_string(),
                )
            } else {
                (
                    Verification::Mismatch,
                    format!(
                        "the element now holds {:?}; check before trying again",
                        truncate(got.as_deref().unwrap_or(""), 200)
                    ),
                )
            };
            return Ok(ActionResult {
                verification,
                detail: Some(detail),
                state: r,
            });
        }
        let (r, changed) = self.settle(plugin, &app, &before_tree, &windows)?;
        let (verification, detail) = match (changed, keys) {
            (true, false) if r.window.is_empty() && r.tree.starts_with("The window closed") => {
                (Verification::Unverified, "the window closed")
            }
            (true, false) => (Verification::Unverified, "the window changed"),
            (true, true) => (Verification::Unverified, "keys sent; the window changed"),
            (false, true) => (
                Verification::Unverified,
                "keys sent; nothing in the window changed",
            ),
            (false, false) => (
                Verification::SuspectedNoop,
                "nothing in the window changed within a second; do not repeat the same click",
            ),
        };
        Ok(ActionResult {
            verification,
            detail: Some(detail.into()),
            state: r,
        })
    }

    /// The items under a menu bar item or menu item: their menu's
    /// children, or their own.
    fn menu_items(&self, node: &B::Node) -> Result<Vec<(B::Node, NodeInfo)>, CallError> {
        let mut out = Vec::new();
        for c in self.backend.children(node)? {
            let info = self.backend.info(&c)?;
            if info.kind == Kind::Menu {
                for item in self.backend.children(&c)? {
                    let i = self.backend.info(&item)?;
                    out.push((item, i));
                }
            } else {
                out.push((c, info));
            }
        }
        Ok(out)
    }

    fn pick_menu(&self, app: &AxApp, path: &[String]) -> Result<(), CallError> {
        if path.is_empty() {
            return Err(CallError::Failed(
                "invalid_argument: the menu path is empty".into(),
            ));
        }
        let bar = self.backend.menu_bar(app.pid).ok_or_else(|| {
            CallError::Failed(format!("menu_not_found: {} has no menu bar", app.name))
        })?;
        let mut current = bar;
        for (depth, want) in path.iter().enumerate() {
            let items = self.menu_items(&current)?;
            let found = items.iter().find(|(_, info)| {
                info.name
                    .as_deref()
                    .is_some_and(|n| menu_name(n) == menu_name(want))
            });
            let Some((node, info)) = found else {
                let names: Vec<String> = items
                    .iter()
                    .filter_map(|(_, i)| i.name.clone())
                    .filter(|n| !n.is_empty())
                    .collect();
                return Err(CallError::Failed(format!(
                    "menu_not_found: no {want:?} in {}; it has [{}]",
                    if depth == 0 {
                        "the menu bar".to_string()
                    } else {
                        format!("{:?}", path[depth - 1])
                    },
                    names.join(", ")
                )));
            };
            if !info.enabled {
                return Err(CallError::Failed(format!(
                    "element_not_enabled: the menu item {want:?} is disabled"
                )));
            }
            if depth + 1 == path.len() {
                return self.backend.press(node);
            }
            self.backend.expand(node)?;
            current = node.clone();
        }
        Ok(())
    }
}

/// "Export…", "Export..." and "export" match.
fn menu_name(name: &str) -> String {
    name.trim()
        .trim_end_matches('…')
        .trim_end_matches("...")
        .trim()
        .to_lowercase()
}

/// Sliders read back "50" for "50.0".
fn same_number(a: &str, b: &str) -> bool {
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(x), Ok(y)) => (x - y).abs() < 1e-6,
        _ => false,
    }
}

struct Walk<'a, B: AxBackend> {
    backend: &'a B,
    cache: &'a mut AppCache<B::Node>,
    lines: Vec<Line>,
    elements: Vec<(u32, NodeInfo, B::Node)>,
    truncated: bool,
}

impl<B: AxBackend> Walk<'_, B> {
    fn visit(
        &mut self,
        node: &B::Node,
        depth: usize,
        parent: Option<usize>,
    ) -> Result<(), CallError> {
        let info = self.backend.info(node)?;
        let index = self.cache.index(info.identity, node);
        let elide = render::should_elide(&info);
        let (child_depth, child_parent) = if elide {
            (depth, parent)
        } else {
            self.lines.push(Line {
                depth,
                text: render::line(index, &info),
                parent,
                index: Some(index),
            });
            (depth + 1, Some(self.lines.len() - 1))
        };
        let suppress = render::should_suppress_children(&info);
        self.elements.push((index, info, node.clone()));
        if suppress {
            return Ok(());
        }
        let children = self.backend.children(node).unwrap_or_default();
        for (n, c) in children.iter().enumerate() {
            if self.elements.len() >= MAX_NODES || child_depth >= MAX_DEPTH {
                self.truncated = true;
                self.lines.push(Line {
                    depth: child_depth,
                    text: format!("... {} more not shown (snapshot limit)", children.len() - n),
                    parent: child_parent,
                    index: None,
                });
                break;
            }
            // An element that went away mid-read is skipped, not fatal.
            match self.visit(c, child_depth, child_parent) {
                Ok(()) => {}
                Err(CallError::Failed(e)) if e.starts_with("element_not_found") => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// The lines, indented two spaces per level. With `query`, only lines
/// containing it (any case) and their ancestors.
fn render_lines(lines: &[Line], query: Option<&str>) -> String {
    let keep: Vec<bool> = match query.map(str::trim).filter(|q| !q.is_empty()) {
        None => vec![true; lines.len()],
        Some(q) => {
            let q = q.to_lowercase();
            let mut keep = vec![false; lines.len()];
            for (i, l) in lines.iter().enumerate() {
                if l.index.is_some() && l.text.to_lowercase().contains(&q) {
                    let mut at = Some(i);
                    while let Some(j) = at {
                        if keep[j] {
                            break;
                        }
                        keep[j] = true;
                        at = lines[j].parent;
                    }
                }
            }
            keep
        }
    };
    let mut out = String::new();
    for (l, k) in lines.iter().zip(keep) {
        if k {
            for _ in 0..l.depth {
                out.push_str("  ");
            }
            out.push_str(&l.text);
            out.push('\n');
        }
    }
    if out.is_empty() && query.is_some() {
        out.push_str("No element matches the query.\n");
    }
    out
}

fn clean(s: Option<&str>) -> Option<String> {
    let s = render::sanitize(s?);
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The line rules, ported from Orca's `SnapshotRenderHeuristics`.
pub mod render {
    use super::{Kind, MAX_VALUE, NodeInfo, clean, truncate};

    /// Actions every control of their kind has, so lines leave them out.
    const NOISY: &[&str] = &[
        "press",
        "click",
        "activate",
        "invoke",
        "show default ui",
        "show alternate ui",
        "show menu",
        "scroll to visible",
        "confirm",
        "raise",
    ];

    pub fn sanitize(value: &str) -> String {
        value.replace(['\n', '\r'], " ")
    }

    fn markdown_escaped(value: &str) -> String {
        sanitize(value)
            .replace('\\', "\\\\")
            .replace('[', "\\[")
            .replace(']', "\\]")
    }

    pub fn display_name(node: &NodeInfo) -> Option<String> {
        if let Some(t) = clean(node.name.as_deref()) {
            return Some(t);
        }
        if node.kind == Kind::Link
            && let (Some(url), Some(text)) = (
                clean(node.url.as_deref()),
                clean(node.description.as_deref()).or_else(|| clean(node.value.as_deref())),
            )
        {
            return Some(format!("[{}]({url})", markdown_escaped(&text)));
        }
        if node.kind == Kind::WebArea {
            return clean(node.description.as_deref()).or_else(|| clean(node.value.as_deref()));
        }
        clean(node.description.as_deref())
    }

    pub fn meaningful_actions(node: &NodeInfo) -> Vec<String> {
        node.actions
            .iter()
            .filter(|a| {
                let a = a.to_lowercase();
                if NOISY.contains(&a.as_str()) {
                    return false;
                }
                if matches!(node.kind, Kind::Menu | Kind::MenuItem)
                    && (a == "cancel" || a == "pick")
                {
                    return false;
                }
                if matches!(node.kind, Kind::Row | Kind::Tab | Kind::Cell) && a == "select" {
                    return false;
                }
                if matches!(node.kind, Kind::CheckBox | Kind::RadioButton) && a == "toggle" {
                    return false;
                }
                true
            })
            .cloned()
            .collect()
    }

    pub fn traits(node: &NodeInfo) -> Vec<&'static str> {
        let mut t = Vec::new();
        if !node.enabled {
            t.push("disabled");
        }
        if node.selected {
            t.push("selected");
        }
        if node.focused {
            t.push("focused");
        }
        match node.expanded {
            Some(true) => t.push("expanded"),
            Some(false) => t.push("collapsed"),
            None => {}
        }
        t
    }

    pub fn should_elide(node: &NodeInfo) -> bool {
        matches!(node.kind, Kind::Group | Kind::Other)
            && display_name(node).is_none()
            && traits(node).is_empty()
            && meaningful_actions(node).is_empty()
            && clean(node.value.as_deref()).is_none()
    }

    const COMPACT: &[Kind] = &[
        Kind::Button,
        Kind::CheckBox,
        Kind::ComboBox,
        Kind::Disclosure,
        Kind::Heading,
        Kind::MenuItem,
        Kind::PopUpButton,
        Kind::RadioButton,
        Kind::StaticText,
        Kind::Tab,
    ];

    pub fn should_suppress_children(node: &NodeInfo) -> bool {
        if node.kind == Kind::MenuBarItem {
            return true;
        }
        let name = display_name(node);
        if node.kind == Kind::Link && name.as_deref().is_some_and(|n| n.starts_with('[')) {
            return true;
        }
        let compact_label = name.is_some() || clean(node.value.as_deref()).is_some();
        compact_label && COMPACT.contains(&node.kind)
    }

    fn value_segment(node: &NodeInfo, name: Option<&str>) -> Option<String> {
        let value = clean(node.value.as_deref())?;
        if Some(value.as_str()) == name {
            return None;
        }
        if node.kind == Kind::Heading && value.parse::<i64>().is_ok() {
            return None;
        }
        let value = truncate(&value, MAX_VALUE);
        if matches!(
            node.kind,
            Kind::StaticText | Kind::TextArea | Kind::ScrollBar
        ) {
            Some(format!(" {value}"))
        } else {
            Some(format!(", Value: {value}"))
        }
    }

    /// `12 button (disabled) Save, Description: …, Value: …, Secondary
    /// Actions: increment, decrement`.
    pub fn line(index: u32, node: &NodeInfo) -> String {
        let name = display_name(node);
        let role = if matches!(node.kind, Kind::Group | Kind::Other) {
            "container".to_string()
        } else if node.kind == Kind::MenuBarItem {
            "menu".to_string()
        } else {
            node.role.to_lowercase()
        };
        let mut line = format!("{index} {role}");
        let traits = traits(node);
        if !traits.is_empty() {
            line.push_str(&format!(" ({})", traits.join(", ")));
        }
        if let Some(n) = &name {
            line.push(' ');
            line.push_str(&truncate(n, MAX_VALUE));
        }
        if node.kind != Kind::Link
            && let Some(d) = clean(node.description.as_deref())
            && Some(&d) != name.as_ref()
        {
            line.push_str(&format!(", Description: {}", truncate(&d, MAX_VALUE)));
        }
        if let Some(v) = value_segment(node, name.as_deref()) {
            line.push_str(&v);
        }
        if let Some(p) = clean(node.placeholder.as_deref())
            && Some(&p) != name.as_ref()
            && Some(p.as_str()) != node.value.as_deref()
        {
            if name.is_none() && clean(node.value.as_deref()).is_none() {
                line.push_str(&format!(" Placeholder: {p}"));
            } else {
                line.push_str(&format!(", Placeholder: {p}"));
            }
        }
        let actions = meaningful_actions(node);
        if !actions.is_empty() {
            line.push_str(&format!(", Secondary Actions: {}", actions.join(", ")));
        }
        line
    }

    /// "AXScrollUpByPage" → "scroll up"; a custom action's
    /// "Name:Copy\nTarget:…" → "Copy".
    pub fn pretty_action(action: &str) -> String {
        if action == "AXZoomWindow" {
            return "zoom the window".into();
        }
        if let Some(rest) = action.strip_prefix("Name:") {
            let name = rest.lines().next().unwrap_or("").trim();
            if !name.is_empty() {
                return sanitize(name);
            }
        }
        let stripped = action.strip_prefix("AX").unwrap_or(action);
        sanitize(&split_camel(&stripped.replace("ByPage", ""))).to_lowercase()
    }

    /// "TextField" → "Text Field".
    pub fn split_camel(value: &str) -> String {
        let mut out = String::new();
        for c in value.chars() {
            if c.is_uppercase() && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push(c);
        }
        out
    }
}

#[async_trait]
impl<B: AxBackend> AccessibilityProvider for Accessibility<B> {
    async fn list_apps(&self, _: &Caller) -> Result<Vec<AxApp>, CallError> {
        self.blocking(|inner| {
            let mut apps = inner.backend.apps()?;
            apps.sort_by_key(|a| !a.frontmost);
            Ok(apps)
        })
        .await
    }

    async fn list_windows(&self, _: &Caller, app: String) -> Result<Vec<AxWindow>, CallError> {
        self.blocking(move |inner| {
            let app = inner.app(&app)?;
            let windows = inner.backend.windows(app.pid)?;
            Ok(windows
                .into_iter()
                .enumerate()
                .map(|(i, (_, w))| AxWindow {
                    index: i as u32,
                    title: w.title,
                    frame: w.frame,
                    focused: w.focused,
                    minimized: w.minimized,
                })
                .collect())
        })
        .await
    }

    async fn snapshot(
        &self,
        caller: &Caller,
        app: String,
        window: Option<u32>,
        query: Option<String>,
        element: Option<u32>,
        screenshot: Option<bool>,
    ) -> Result<AppState, CallError> {
        let plugin = caller.plugin.clone();
        let (mut state, frame, minimized) = self
            .blocking(move |inner| {
                let app = inner.app(&app)?;
                let read = inner.read(&plugin, &app, window, element)?;
                Ok((
                    read.state(query.as_deref()),
                    read.window.frame,
                    read.window.minimized,
                ))
            })
            .await?;
        if screenshot.unwrap_or(false) {
            let screen = self.inner.screen.as_ref().ok_or(CallError::Unavailable)?;
            let frame = frame.filter(|_| !minimized).ok_or_else(|| {
                CallError::Failed(
                    "screenshot_failed: the window is minimized or has no frame".into(),
                )
            })?;
            state.screenshot = Some(screen.capture(caller, Some(frame)).await?);
        }
        Ok(state)
    }

    async fn find(
        &self,
        caller: &Caller,
        app: String,
        text: String,
    ) -> Result<Vec<Element>, CallError> {
        let plugin = caller.plugin.clone();
        self.blocking(move |inner| {
            let app = inner.app(&app)?;
            Ok(inner.read(&plugin, &app, None, None)?.matching(&text))
        })
        .await
    }

    async fn click(
        &self,
        caller: &Caller,
        app: String,
        element: u32,
    ) -> Result<ActionResult, CallError> {
        self.act(caller, app, Act::Click(element)).await
    }

    async fn set_value(
        &self,
        caller: &Caller,
        app: String,
        element: u32,
        value: String,
    ) -> Result<ActionResult, CallError> {
        self.act(caller, app, Act::SetValue(element, value)).await
    }

    async fn type_text(
        &self,
        caller: &Caller,
        app: String,
        element: Option<u32>,
        text: String,
    ) -> Result<ActionResult, CallError> {
        self.act(caller, app, Act::Type(element, text)).await
    }

    async fn press_key(
        &self,
        caller: &Caller,
        app: String,
        key: String,
    ) -> Result<ActionResult, CallError> {
        self.act(caller, app, Act::Key(key)).await
    }

    async fn perform(
        &self,
        caller: &Caller,
        app: String,
        element: u32,
        action: String,
    ) -> Result<ActionResult, CallError> {
        self.act(caller, app, Act::Perform(element, action)).await
    }

    async fn menu(
        &self,
        caller: &Caller,
        app: String,
        path: Vec<String>,
    ) -> Result<ActionResult, CallError> {
        self.act(caller, app, Act::Menu(path)).await
    }

    async fn wait(
        &self,
        caller: &Caller,
        app: String,
        text: String,
        timeout_ms: Option<u32>,
    ) -> Result<Vec<Element>, CallError> {
        let timeout = timeout_ms
            .map(|ms| Duration::from_millis(ms.into()))
            .unwrap_or(WAIT_DEFAULT)
            .min(WAIT_MAX);
        let start = Instant::now();
        loop {
            let plugin = caller.plugin.clone();
            let (app_c, text_c) = (app.clone(), text.clone());
            // The app or window may not exist yet: keep looking.
            let found = self
                .blocking(move |inner| {
                    let app = inner.app(&app_c)?;
                    Ok(inner.read(&plugin, &app, None, None)?.matching(&text_c))
                })
                .await
                .unwrap_or_default();
            if !found.is_empty() {
                return Ok(found);
            }
            if start.elapsed() >= timeout {
                return Err(CallError::Timeout);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    fn release(&self, plugin: &str) {
        self.inner
            .caches
            .lock()
            .unwrap()
            .retain(|(p, _), _| p != plugin);
    }
}

/// An in-memory desktop for tests and for trying agents without touching
/// a real app. Elements are numbered; pressing one runs its handler.
pub mod fake {
    use std::sync::{Arc, Mutex};

    use super::*;

    pub type Handler = Arc<dyn Fn(&mut FakeDesktop, usize) + Send + Sync>;

    #[derive(Clone)]
    pub struct FakeNode {
        pub info: NodeInfo,
        pub children: Vec<usize>,
        pub on_press: Option<Handler>,
        pub settable: bool,
    }

    /// The desktop: apps, their windows and menu bars, as node ids.
    #[derive(Clone, Default)]
    pub struct FakeDesktop {
        pub nodes: Vec<FakeNode>,
        pub apps: Vec<(AxApp, Vec<usize>, Option<usize>)>,
        pub focus: Option<usize>,
        /// Chords pressed, as text, per pid.
        pub keys: Vec<(i32, String)>,
        /// Nodes removed from the desktop.
        pub gone: Vec<usize>,
    }

    impl FakeDesktop {
        pub fn add(
            &mut self,
            kind: Kind,
            role: &str,
            name: Option<&str>,
            parent: Option<usize>,
        ) -> usize {
            let id = self.nodes.len();
            let mut info = NodeInfo::new(kind, role, id as u64 + 1000);
            info.name = name.map(str::to_string);
            if matches!(
                kind,
                Kind::Button | Kind::MenuItem | Kind::CheckBox | Kind::MenuBarItem
            ) {
                info.actions.push("press".into());
            }
            self.nodes.push(FakeNode {
                info,
                children: Vec::new(),
                on_press: None,
                settable: kind.editable() || kind == Kind::Slider,
            });
            if let Some(p) = parent {
                self.nodes[p].children.push(id);
            }
            id
        }

        pub fn on_press(
            &mut self,
            id: usize,
            f: impl Fn(&mut FakeDesktop, usize) + Send + Sync + 'static,
        ) {
            self.nodes[id].on_press = Some(Arc::new(f));
        }
    }

    /// The backend over a shared [`FakeDesktop`].
    #[derive(Clone, Default)]
    pub struct FakeBackend(pub Arc<Mutex<FakeDesktop>>);

    fn gone(d: &FakeDesktop, id: usize) -> Result<(), CallError> {
        if d.gone.contains(&id) || id >= d.nodes.len() {
            Err(CallError::Failed(
                "element_not_found: the element is gone".into(),
            ))
        } else {
            Ok(())
        }
    }

    impl AxBackend for FakeBackend {
        type Node = usize;

        fn apps(&self) -> Result<Vec<AxApp>, CallError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .apps
                .iter()
                .map(|(a, _, _)| a.clone())
                .collect())
        }

        fn windows(&self, pid: i32) -> Result<Vec<(usize, WindowMeta)>, CallError> {
            let d = self.0.lock().unwrap();
            let Some((_, windows, _)) = d.apps.iter().find(|(a, _, _)| a.pid == pid) else {
                return Ok(Vec::new());
            };
            Ok(windows
                .iter()
                .filter(|w| !d.gone.contains(w))
                .enumerate()
                .map(|(i, &w)| {
                    let info = &d.nodes[w].info;
                    (
                        w,
                        WindowMeta {
                            title: info.name.clone().unwrap_or_default(),
                            frame: info.frame,
                            focused: i == 0,
                            minimized: false,
                        },
                    )
                })
                .collect())
        }

        fn info(&self, node: &usize) -> Result<NodeInfo, CallError> {
            let d = self.0.lock().unwrap();
            gone(&d, *node)?;
            let mut info = d.nodes[*node].info.clone();
            info.focused = d.focus == Some(*node);
            Ok(info)
        }

        fn children(&self, node: &usize) -> Result<Vec<usize>, CallError> {
            let d = self.0.lock().unwrap();
            gone(&d, *node)?;
            Ok(d.nodes[*node]
                .children
                .iter()
                .copied()
                .filter(|c| !d.gone.contains(c))
                .collect())
        }

        fn focused(&self, _: i32) -> Option<usize> {
            self.0.lock().unwrap().focus
        }

        fn menu_bar(&self, pid: i32) -> Option<usize> {
            let d = self.0.lock().unwrap();
            d.apps
                .iter()
                .find(|(a, _, _)| a.pid == pid)
                .and_then(|(_, _, m)| *m)
        }

        fn press(&self, node: &usize) -> Result<(), CallError> {
            let handler = {
                let d = self.0.lock().unwrap();
                gone(&d, *node)?;
                d.nodes[*node].on_press.clone()
            };
            let Some(h) = handler else {
                return Err(CallError::Failed(
                    "element_not_clickable: the element has no press action".into(),
                ));
            };
            let mut d = self.0.lock().unwrap();
            h(&mut d, *node);
            Ok(())
        }

        fn perform(&self, node: &usize, action: &str) -> Result<(), CallError> {
            if action == "press" {
                return self.press(node);
            }
            let mut d = self.0.lock().unwrap();
            gone(&d, *node)?;
            let n = &mut d.nodes[*node].info;
            match action {
                "increment" => {
                    let v: i64 = n.value.as_deref().unwrap_or("0").parse().unwrap_or(0);
                    n.value = Some((v + 1).to_string());
                    Ok(())
                }
                _ => Err(CallError::Failed(format!("action_not_supported: {action}"))),
            }
        }

        fn set_value(&self, node: &usize, value: &str) -> Result<(), CallError> {
            let mut d = self.0.lock().unwrap();
            gone(&d, *node)?;
            if !d.nodes[*node].settable {
                return Err(CallError::Failed("value_not_settable".into()));
            }
            d.nodes[*node].info.value = Some(value.to_string());
            Ok(())
        }

        fn insert_text(&self, node: &usize, text: &str) -> Result<(), CallError> {
            let mut d = self.0.lock().unwrap();
            gone(&d, *node)?;
            let v = d.nodes[*node].info.value.get_or_insert_default();
            v.push_str(text);
            Ok(())
        }

        fn press_key(&self, pid: i32, chord: &Chord) -> Result<(), CallError> {
            self.0.lock().unwrap().keys.push((pid, chord.to_string()));
            Ok(())
        }
    }
}

/// Whether `mods` hold cmd, for backends that map chords.
pub fn has(chord: &Chord, m: Modifier) -> bool {
    chord.mods.contains(&m)
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    /// A calculator: a window with a display and keys 1, 2, +, =, and a
    /// menu bar with Edit > Clear… and a disabled Edit > Undo.
    fn calculator() -> (FakeBackend, usize, usize, usize) {
        let mut d = FakeDesktop::default();
        let win = d.add(Kind::Window, "window", Some("Calculator"), None);
        let group = d.add(Kind::Group, "group", None, Some(win));
        let display = d.add(Kind::StaticText, "text", None, Some(group));
        d.nodes[display].info.value = Some("0".into());
        let mut keys = Vec::new();
        for k in ["1", "2", "+", "="] {
            let b = d.add(Kind::Button, "button", Some(k), Some(group));
            keys.push(b);
            d.on_press(b, move |d, id| {
                let key = d.nodes[id].info.name.clone().unwrap();
                let shown = d.nodes[display].info.value.clone().unwrap();
                let next = match key.as_str() {
                    "=" => {
                        let sum: i64 = shown
                            .split('+')
                            .map(|n| n.parse::<i64>().unwrap_or(0))
                            .sum();
                        sum.to_string()
                    }
                    k if shown == "0" => k.to_string(),
                    k => format!("{shown}{k}"),
                };
                d.nodes[display].info.value = Some(next);
            });
        }
        let field = d.add(Kind::TextField, "text field", Some("Memo"), Some(win));
        d.nodes[field].info.value = Some(String::new());
        let disabled = d.add(Kind::Button, "button", Some("Pay"), Some(win));
        d.nodes[disabled].info.enabled = false;
        let silent = d.add(Kind::Button, "button", Some("Nothing"), Some(win));
        d.on_press(silent, |_, _| {});

        let bar = d.add(Kind::MenuBar, "menu bar", None, None);
        let edit = d.add(Kind::MenuBarItem, "menu bar item", Some("Edit"), Some(bar));
        let menu = d.add(Kind::Menu, "menu", None, Some(edit));
        let clear = d.add(Kind::MenuItem, "menu item", Some("Clear…"), Some(menu));
        d.on_press(clear, move |d, _| {
            d.nodes[display].info.value = Some("0".into())
        });
        let undo = d.add(Kind::MenuItem, "menu item", Some("Undo"), Some(menu));
        d.nodes[undo].info.enabled = false;
        d.apps.push((
            AxApp {
                name: "Calculator".into(),
                bundle_id: Some("com.apple.calculator".into()),
                pid: 42,
                frontmost: true,
            },
            vec![win],
            Some(bar),
        ));
        d.apps.push((
            AxApp {
                name: "Calendar".into(),
                bundle_id: Some("com.apple.iCal".into()),
                pid: 7,
                frontmost: false,
            },
            Vec::new(),
            None,
        ));
        (FakeBackend(Arc::new(Mutex::new(d))), display, field, silent)
    }

    fn engine(b: &FakeBackend) -> Accessibility<FakeBackend> {
        Accessibility::with_blocked(b.clone(), None, BLOCKED_MACOS)
    }

    fn index_of(tree: &str, text: &str) -> u32 {
        let line = tree
            .lines()
            .find(|l| l.contains(text))
            .unwrap_or_else(|| panic!("no {text:?} in\n{tree}"));
        line.split_whitespace().next().unwrap().parse().unwrap()
    }

    #[test]
    fn picks_apps_by_pid_bundle_and_name() {
        let apps = vec![
            AxApp {
                name: "Safari".into(),
                bundle_id: Some("com.apple.Safari".into()),
                pid: 1,
                frontmost: false,
            },
            AxApp {
                name: "Safari Technology Preview".into(),
                bundle_id: None,
                pid: 2,
                frontmost: false,
            },
            AxApp {
                name: "Notes".into(),
                bundle_id: None,
                pid: 3,
                frontmost: false,
            },
        ];
        assert_eq!(pick_app(&apps, "pid:2").unwrap().pid, 2);
        assert_eq!(pick_app(&apps, "COM.APPLE.SAFARI").unwrap().pid, 1);
        assert_eq!(pick_app(&apps, "safari").unwrap().pid, 1);
        assert_eq!(pick_app(&apps, "note").unwrap().pid, 3);
        // "Saf" matches two names: no guess.
        assert!(pick_app(&apps, "saf").is_none());
        assert!(pick_app(&apps, "pid:x").is_none());
    }

    #[tokio::test]
    async fn snapshot_renders_lines_with_indexes_and_elides_groups() {
        let (b, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        let s = ax
            .snapshot(&c, "calculator".into(), None, None, None, None)
            .await
            .unwrap();
        assert_eq!(s.app, "Calculator");
        assert_eq!(s.window, "Calculator");
        assert!(!s.truncated);
        let lines: Vec<&str> = s.tree.lines().collect();
        assert_eq!(lines[0], "1 window Calculator");
        // The unnamed group is elided: its children sit one level down.
        assert_eq!(lines[1], "  3 text 0");
        assert_eq!(lines[2], "  4 button 1");
        assert!(s.tree.contains("text field Memo, Value: ") || s.tree.contains("text field Memo"));
        assert!(s.tree.contains("button (disabled) Pay"));
        assert!(!s.tree.contains("container"));
        // Indexes stay put across reads.
        let again = ax
            .snapshot(&c, "com.apple.calculator".into(), None, None, None, None)
            .await
            .unwrap();
        assert_eq!(again.tree, s.tree);
    }

    #[tokio::test]
    async fn query_keeps_matches_and_their_ancestors() {
        let (b, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        let s = ax
            .snapshot(
                &c,
                "Calculator".into(),
                None,
                Some("MEMO".into()),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(s.tree, "1 window Calculator\n  8 text field Memo\n");
        let none = ax
            .snapshot(
                &c,
                "Calculator".into(),
                None,
                Some("zzz".into()),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(none.tree, "No element matches the query.\n");
    }

    #[tokio::test]
    async fn clicks_and_reads_the_result() {
        let (b, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("agent");
        let s = ax
            .snapshot(&c, "Calculator".into(), None, None, None, None)
            .await
            .unwrap();
        for key in ["button 1", "button +", "button 2", "button ="] {
            let r = ax
                .click(&c, "Calculator".into(), index_of(&s.tree, key))
                .await
                .unwrap();
            assert_eq!(r.verification, Verification::Unverified, "{key}");
            assert_eq!(r.detail.as_deref(), Some("the window changed"));
        }
        let found = ax
            .find(&c, "Calculator".into(), "text".into())
            .await
            .unwrap();
        assert_eq!(found[0].value.as_deref(), Some("3"));
    }

    #[tokio::test]
    async fn a_click_that_closes_the_window_is_a_result() {
        let (b, ..) = calculator();
        let ok = {
            let mut d = b.0.lock().unwrap();
            let win = d.apps[0].1[0];
            let ok = d.add(Kind::Button, "button", Some("OK"), Some(win));
            d.on_press(ok, move |d, _| d.gone.push(win));
            ok
        };
        let _ = ok;
        let ax = engine(&b);
        let c = Caller::new("t");
        let s = ax
            .snapshot(&c, "Calculator".into(), None, None, None, None)
            .await
            .unwrap();
        let r = ax
            .click(&c, "Calculator".into(), index_of(&s.tree, "button OK"))
            .await
            .unwrap();
        assert_eq!(r.detail.as_deref(), Some("the window closed"));
        assert_eq!(r.state.tree, "The window closed.\n");
    }

    #[tokio::test]
    async fn a_click_that_changes_nothing_is_a_suspected_noop() {
        let (b, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        let s = ax
            .snapshot(&c, "Calculator".into(), None, None, None, None)
            .await
            .unwrap();
        let r = ax
            .click(&c, "Calculator".into(), index_of(&s.tree, "Nothing"))
            .await
            .unwrap();
        assert_eq!(r.verification, Verification::SuspectedNoop);
    }

    #[tokio::test]
    async fn refuses_disabled_stale_and_unknown_elements() {
        let (b, display, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        // No snapshot yet: every index is unknown to this caller.
        let e = ax.click(&c, "Calculator".into(), 4).await.unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("element_not_found")));
        let s = ax
            .snapshot(&c, "Calculator".into(), None, None, None, None)
            .await
            .unwrap();
        let e = ax
            .click(&c, "Calculator".into(), index_of(&s.tree, "Pay"))
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("element_not_enabled")));
        // Another caller has its own indexes.
        let e = ax
            .click(&Caller::new("other"), "Calculator".into(), 4)
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("element_not_found")));
        // The display goes away: its index is stale.
        b.0.lock().unwrap().gone.push(display);
        let e = ax
            .set_value(&c, "Calculator".into(), 3, "9".into())
            .await
            .unwrap_err();
        assert!(
            matches!(&e, CallError::Failed(m) if m.starts_with("element_not_found")),
            "{e:?}"
        );
        let e = ax
            .snapshot(&c, "Nope".into(), None, None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("app_not_found")));
        let e = ax
            .snapshot(&c, "Calendar".into(), None, None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("window_not_found")));
    }

    #[tokio::test]
    async fn set_value_and_type_text_read_back() {
        let (b, _, field, _) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        let s = ax
            .snapshot(&c, "Calculator".into(), None, None, None, None)
            .await
            .unwrap();
        let memo = index_of(&s.tree, "Memo");
        let r = ax
            .set_value(&c, "Calculator".into(), memo, "groceries".into())
            .await
            .unwrap();
        assert_eq!(r.verification, Verification::Verified);
        assert!(r.state.tree.contains("Memo, Value: groceries"));
        let r = ax
            .type_text(&c, "Calculator".into(), Some(memo), " and milk".into())
            .await
            .unwrap();
        assert_eq!(r.verification, Verification::Verified);
        assert_eq!(
            b.0.lock().unwrap().nodes[field].info.value.as_deref(),
            Some("groceries and milk")
        );
        // Without an element, text goes to the focused one; none here.
        let e = ax
            .type_text(&c, "Calculator".into(), None, "x".into())
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("no_focus")));
        b.0.lock().unwrap().focus = Some(field);
        let r = ax
            .type_text(&c, "Calculator".into(), None, "!".into())
            .await
            .unwrap();
        assert_eq!(r.verification, Verification::Verified);
        assert_eq!(r.state.focused, Some(memo));
        // A button takes no text.
        let e = ax
            .type_text(
                &c,
                "Calculator".into(),
                Some(index_of(&s.tree, "button 1")),
                "x".into(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("value_not_settable")));
    }

    #[tokio::test]
    async fn menus_match_names_loosely_and_refuse_disabled_items() {
        let (b, display, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        b.0.lock().unwrap().nodes[display].info.value = Some("12".into());
        let r = ax
            .menu(
                &c,
                "Calculator".into(),
                vec!["edit".into(), "Clear...".into()],
            )
            .await
            .unwrap();
        assert!(r.state.tree.contains("text 0"), "{}", r.state.tree);
        let e = ax
            .menu(&c, "Calculator".into(), vec!["Edit".into(), "Undo".into()])
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("element_not_enabled")));
        let e = ax
            .menu(&c, "Calculator".into(), vec!["File".into()])
            .await
            .unwrap_err();
        assert!(
            matches!(&e, CallError::Failed(m) if m.contains("[Edit]")),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn press_key_blocks_quit_and_close_chords() {
        let (b, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        let e = ax
            .press_key(&c, "Calculator".into(), "cmd+q".into())
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("system_shortcut_blocked")));
        let e = ax
            .press_key(&c, "Calculator".into(), "cmd+option+escape".into())
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("system_shortcut_blocked")));
        let r = ax
            .press_key(&c, "Calculator".into(), "CmdOrCtrl+A".into())
            .await
            .unwrap();
        assert_eq!(r.verification, Verification::Unverified);
        let r = ax
            .press_key(&c, "Calculator".into(), "return".into())
            .await
            .unwrap();
        assert_eq!(
            r.detail.as_deref(),
            Some("keys sent; nothing in the window changed")
        );
        let keys = b.0.lock().unwrap().keys.clone();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1], (42, "Enter".to_string()));
        assert!(parse_key("alt+f4", BLOCKED_WINDOWS).is_err());
        assert!(parse_key("ctrl+q", BLOCKED_LINUX).is_err());
        assert!(parse_key("hyper+q", BLOCKED_LINUX).is_err());
    }

    #[tokio::test]
    async fn perform_takes_only_listed_actions() {
        let (b, ..) = calculator();
        let slider = {
            let mut d = b.0.lock().unwrap();
            let win = d.apps[0].1[0];
            let s = d.add(Kind::Slider, "slider", Some("Volume"), Some(win));
            d.nodes[s].info.value = Some("5".into());
            d.nodes[s].info.actions = vec!["increment".into(), "decrement".into()];
            s
        };
        let ax = engine(&b);
        let c = Caller::new("t");
        let s = ax
            .snapshot(&c, "Calculator".into(), None, None, None, None)
            .await
            .unwrap();
        assert!(
            s.tree
                .contains("slider Volume, Value: 5, Secondary Actions: increment, decrement")
        );
        let i = index_of(&s.tree, "Volume");
        ax.perform(&c, "Calculator".into(), i, "Increment".into())
            .await
            .unwrap();
        assert_eq!(
            b.0.lock().unwrap().nodes[slider].info.value.as_deref(),
            Some("6")
        );
        let e = ax
            .perform(&c, "Calculator".into(), i, "explode".into())
            .await
            .unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.contains("[increment, decrement]")));
        let r = ax.set_value(&c, "Calculator".into(), i, "7.0".into()).await;
        // The fake stores the text as sent; "7.0" reads back as 7.0.
        assert_eq!(r.unwrap().verification, Verification::Verified);
    }

    #[tokio::test]
    async fn large_trees_stop_at_the_limit_with_a_marker() {
        let mut d = FakeDesktop::default();
        let win = d.add(Kind::Window, "window", Some("Big"), None);
        let list = d.add(Kind::List, "list", Some("Rows"), Some(win));
        for i in 0..(MAX_NODES + 50) {
            d.add(
                Kind::StaticText,
                "text",
                Some(&format!("row {i}")),
                Some(list),
            );
        }
        d.apps.push((
            AxApp {
                name: "Big".into(),
                bundle_id: None,
                pid: 1,
                frontmost: true,
            },
            vec![win],
            None,
        ));
        let b = FakeBackend(Arc::new(Mutex::new(d)));
        let ax = engine(&b);
        let c = Caller::new("t");
        let s = ax
            .snapshot(&c, "Big".into(), None, None, None, None)
            .await
            .unwrap();
        assert!(s.truncated);
        assert_eq!(s.element_count as usize, MAX_NODES);
        assert!(
            s.tree
                .contains("    ... 52 more not shown (snapshot limit)"),
            "{}",
            &s.tree[s.tree.len() - 200..]
        );
        // Reading the list alone shows rows the window read could not.
        let sub = ax
            .snapshot(
                &c,
                "Big".into(),
                None,
                Some("row 1198".into()),
                Some(2),
                None,
            )
            .await
            .unwrap();
        assert!(sub.tree.contains("text row 1198") && !s.tree.contains("text row 1198"));
    }

    #[tokio::test]
    async fn wait_returns_once_the_text_shows_or_times_out() {
        let (b, display, ..) = calculator();
        let ax = Arc::new(engine(&b));
        let c = Caller::new("t");
        let e = ax
            .wait(&c, "Calculator".into(), "saved".into(), Some(300))
            .await
            .unwrap_err();
        assert_eq!(e, CallError::Timeout);
        let b2 = b.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            b2.0.lock().unwrap().nodes[display].info.value = Some("Saved".into());
        });
        let found = ax
            .wait(&c, "Calculator".into(), "saved".into(), None)
            .await
            .unwrap();
        assert_eq!(found[0].value.as_deref(), Some("Saved"));
    }

    #[tokio::test]
    async fn release_forgets_a_callers_indexes() {
        let (b, ..) = calculator();
        let ax = engine(&b);
        let c = Caller::new("t");
        ax.snapshot(&c, "Calculator".into(), None, None, None, None)
            .await
            .unwrap();
        ax.release("t");
        let e = ax.click(&c, "Calculator".into(), 4).await.unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("element_not_found")));
    }

    #[test]
    fn renders_lines_like_orca() {
        let mut link = NodeInfo::new(Kind::Link, "link", 1);
        link.description = Some("Docs [v2]".into());
        link.url = Some("https://x.test/d".into());
        assert_eq!(
            render::line(5, &link),
            "5 link [Docs \\[v2\\]](https://x.test/d)"
        );
        assert!(render::should_suppress_children(&link));

        let mut field = NodeInfo::new(Kind::TextField, "search field", 2);
        field.placeholder = Some("Search".into());
        field.focused = true;
        assert_eq!(
            render::line(9, &field),
            "9 search field (focused) Placeholder: Search"
        );

        let mut heading = NodeInfo::new(Kind::Heading, "heading", 3);
        heading.name = Some("Title".into());
        heading.value = Some("2".into());
        assert_eq!(render::line(1, &heading), "1 heading Title");

        let mut group = NodeInfo::new(Kind::Group, "group", 4);
        assert!(render::should_elide(&group));
        group.actions = vec!["press".into(), "scroll up".into()];
        assert!(!render::should_elide(&group));
        assert_eq!(
            render::line(2, &group),
            "2 container, Secondary Actions: scroll up"
        );

        assert_eq!(render::pretty_action("AXScrollUpByPage"), "scroll up");
        assert_eq!(render::pretty_action("Name:Copy\nTarget:0x0"), "Copy");
        assert_eq!(render::pretty_action("AXShowMenu"), "show menu");
        assert_eq!(render::split_camel("TextField"), "Text Field");

        let mut long = NodeInfo::new(Kind::TextArea, "text entry area", 5);
        long.value = Some("a\nb".repeat(300));
        let l = render::line(3, &long);
        assert!(l.ends_with('…') && !l.contains('\n'));
    }
}
