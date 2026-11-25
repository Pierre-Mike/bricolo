//! `accessibility` on AT-SPI: the backend under
//! [`crate::accessibility::Accessibility`]. Apps, windows and elements are
//! read from the accessibility bus (`org.a11y.Bus` on the session bus
//! gives its address) and acted on through their AT-SPI interfaces, so
//! nothing moves the pointer or raises a window: `Action.DoAction` presses,
//! `EditableText` and `Value` set values. Keys are the one exception: they
//! go to the focused window through XTEST, so they are sent only while the
//! app owns the active window, and not at all on Wayland.
//!
//! An element is its bus name and object path. The calls are async D-Bus
//! calls on a small Tokio runtime of this backend's own, driven with
//! `block_on` from the threads the engine runs backend calls on, with the
//! independent reads of one element in flight together.
//!
//! Secure fields (password roles, and entries named like a secret) never
//! show their text.
//!
//! The reading choices follow Orca's computer use (github.com/stablyai/orca,
//! `native/computer-use-linux/runtime.py`): which children are windows,
//! which action presses, inserting text through `EditableText`, and the
//! words that mark a secret field.
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
//! Known limits: GTK 4 reports screen extents of 0,0 for most elements, so
//! frames of its elements can be wrong; menus that a toolkit builds only
//! while open (GTK 4) are not in the tree until opened.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::future::join_all;
use zbus::Connection;
use zbus::zvariant::DynamicType;
use zbus::{OwnedObjectPath, OwnedVariant, Variant};

use super::{Session, warn_once};
use crate::accessibility::{AxApp, AxBackend, Kind, MAX_NODES, NodeInfo, WindowMeta};
use crate::atspi::{self, Hint, NULL_PATH, States, state};
use crate::error::CallError;
use crate::hotkey::Chord;

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const COMPONENT: &str = "org.a11y.atspi.Component";
const ACTION: &str = "org.a11y.atspi.Action";
const TEXT: &str = "org.a11y.atspi.Text";
const EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
const VALUE: &str = "org.a11y.atspi.Value";
const SELECTION: &str = "org.a11y.atspi.Selection";
const HYPERLINK: &str = "org.a11y.atspi.Hyperlink";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// One call to an app that hangs holds the read this long.
const CALL_TIMEOUT: Duration = Duration::from_millis(1500);
/// Levels of a window searched for the menu bar.
const MENU_BAR_DEPTH: usize = 8;
/// Levels searched for the focused element.
const FOCUS_DEPTH: usize = 64;

/// One element: the bus name of its app and its object path.
#[derive(Clone, Debug)]
pub struct Node {
    dest: Arc<str>,
    path: Arc<str>,
    hint: Hint,
}

impl Node {
    fn new(dest: &str, path: &str, hint: Hint) -> Self {
        Self {
            dest: dest.into(),
            path: path.into(),
            hint,
        }
    }
}

pub struct AtSpi {
    /// Started on the first call.
    runtime: OnceLock<tokio::runtime::Runtime>,
    bus: tokio::sync::Mutex<Option<Arc<Bus>>>,
    warned: AtomicBool,
}

impl Default for AtSpi {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AtSpi {
    fn drop(&mut self) {
        self.bus.get_mut().take();
        if let Some(rt) = self.runtime.take() {
            // Dropping a runtime inside async code panics.
            rt.shutdown_background();
        }
    }
}

/// The element is gone.
fn gone() -> CallError {
    CallError::Failed("element_not_found: the element is gone; take a new snapshot".into())
}

fn denied(what: &str) -> CallError {
    CallError::Failed(format!("accessibility_error: {what}"))
}

/// The engine's error for a D-Bus one.
fn map_err(e: &zbus::Error) -> CallError {
    use zbus::fdo::Error as Fdo;
    match e {
        zbus::Error::FDO(f) => match &**f {
            Fdo::ServiceUnknown(_) | Fdo::NameHasNoOwner(_) | Fdo::UnknownObject(_) => gone(),
            Fdo::NoReply(_) | Fdo::Timeout(_) | Fdo::TimedOut(_) => {
                denied("the app did not answer")
            }
            other => denied(&other.to_string()),
        },
        zbus::Error::MethodError(name, detail, _) => {
            let name = name.as_str();
            if ["UnknownObject", "Defunct", "ServiceUnknown", "NoSuchObject"]
                .iter()
                .any(|n| name.contains(n))
            {
                gone()
            } else {
                denied(&format!("{name}: {}", detail.as_deref().unwrap_or("")))
            }
        }
        other => denied(&other.to_string()),
    }
}

/// A connection to the accessibility bus.
struct Bus {
    conn: Connection,
    /// Bus name to process id: unique names are not reused.
    pids: Mutex<HashMap<String, i32>>,
    /// A call failed on the connection itself: open a new one next time.
    dead: AtomicBool,
}

impl Bus {
    async fn call<B, R>(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        method: &str,
        body: &B,
    ) -> Result<R, CallError>
    where
        B: serde::Serialize + DynamicType,
        R: for<'d> zbus::wire::DynamicDeserialize<'d>,
    {
        let call = self
            .conn
            .call_method(Some(dest), path, Some(iface), method, body);
        let reply = match tokio::time::timeout(CALL_TIMEOUT, call).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => {
                let disconnected = match &e {
                    zbus::Error::InputOutput(_) => true,
                    zbus::Error::FDO(f) => matches!(**f, zbus::fdo::Error::Disconnected(_)),
                    _ => false,
                };
                if disconnected {
                    self.dead.store(true, Ordering::Relaxed);
                }
                return Err(map_err(&e));
            }
            Err(_) => return Err(denied("the app did not answer")),
        };
        reply
            .body()
            .deserialize()
            .map_err(|e| denied(&e.to_string()))
    }

    async fn on<B, R>(&self, n: &Node, iface: &str, method: &str, body: &B) -> Result<R, CallError>
    where
        B: serde::Serialize + DynamicType,
        R: for<'d> zbus::wire::DynamicDeserialize<'d>,
    {
        self.call(&n.dest, &n.path, iface, method, body).await
    }

    /// One property, through `org.freedesktop.DBus.Properties`.
    async fn get<T: TryFrom<OwnedVariant>>(
        &self,
        n: &Node,
        iface: &str,
        name: &str,
    ) -> Result<T, CallError> {
        let v: OwnedVariant = self.on(n, PROPERTIES, "Get", &(iface, name)).await?;
        T::try_from(v).map_err(|_| denied(&format!("{name} has an unexpected type")))
    }

    async fn children_raw(&self, n: &Node) -> Result<Vec<(String, String)>, CallError> {
        let kids: Vec<(String, OwnedObjectPath)> =
            self.on(n, ACCESSIBLE, "GetChildren", &()).await?;
        Ok(kids
            .into_iter()
            .map(|(d, p)| (d, p.to_string()))
            .filter(|(d, p)| !d.is_empty() && p != NULL_PATH)
            .collect())
    }

    async fn role(&self, n: &Node) -> Result<String, CallError> {
        self.on(n, ACCESSIBLE, "GetRoleName", &()).await
    }

    async fn states(&self, n: &Node) -> Result<States, CallError> {
        let words: Vec<u32> = self.on(n, ACCESSIBLE, "GetState", &()).await?;
        Ok(States::from_words(&words))
    }

    async fn interfaces(&self, n: &Node) -> Result<Vec<String>, CallError> {
        self.on(n, ACCESSIBLE, "GetInterfaces", &()).await
    }

    async fn name(&self, n: &Node) -> Option<String> {
        let s: String = self.get(n, ACCESSIBLE, "Name").await.ok()?;
        (!s.is_empty()).then_some(s)
    }

    async fn description(&self, n: &Node) -> Option<String> {
        let s: String = self.get(n, ACCESSIBLE, "Description").await.ok()?;
        (!s.is_empty()).then_some(s)
    }

    /// Screen coordinates.
    async fn frame(&self, n: &Node) -> Option<crate::window::Rect> {
        let e: (i32, i32, i32, i32) = self.on(n, COMPONENT, "GetExtents", &(0u32,)).await.ok()?;
        atspi::frame(e)
    }

    /// `(name, description)` of each action.
    async fn actions(&self, n: &Node) -> Vec<(String, String)> {
        let list: Result<Vec<(String, String, String)>, _> =
            self.on(n, ACTION, "GetActions", &()).await;
        list.unwrap_or_default()
            .into_iter()
            .map(|(name, description, _)| (name, description))
            .collect()
    }

    async fn do_action(&self, n: &Node, index: usize) -> Result<(), CallError> {
        let index = i32::try_from(index).map_err(|_| denied("action index out of range"))?;
        let done: bool = self.on(n, ACTION, "DoAction", &(index,)).await?;
        if done {
            Ok(())
        } else {
            Err(denied("the app refused the action"))
        }
    }

    async fn text_value(&self, n: &Node) -> Option<String> {
        let count: i32 = self.get(n, TEXT, "CharacterCount").await.ok()?;
        if count <= 0 {
            return None;
        }
        let end = count.min(atspi::TEXT_LIMIT);
        let text: String = self.on(n, TEXT, "GetText", &(0i32, end)).await.ok()?;
        Some(atspi::clipped(text, count))
    }

    async fn number(&self, n: &Node) -> Option<f64> {
        self.get(n, VALUE, "CurrentValue").await.ok()
    }

    async fn uri(&self, n: &Node) -> Option<String> {
        let uri: String = self.on(n, HYPERLINK, "GetURI", &(0i32,)).await.ok()?;
        (!uri.is_empty()).then_some(uri)
    }

    async fn pid(&self, dest: &str) -> Option<i32> {
        if let Some(p) = self.pids.lock().unwrap().get(dest) {
            return Some(*p);
        }
        let pid: u32 = self
            .call(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetConnectionUnixProcessID",
                &(dest,),
            )
            .await
            .ok()?;
        let pid = i32::try_from(pid).ok()?;
        self.pids.lock().unwrap().insert(dest.to_string(), pid);
        Some(pid)
    }

    /// The apps on the bus: the registry's children.
    async fn app_roots(&self) -> Result<Vec<Node>, CallError> {
        let root = Node::new(REGISTRY, ROOT_PATH, Hint::None);
        Ok(self
            .children_raw(&root)
            .await?
            .iter()
            .map(|(d, p)| Node::new(d, p, Hint::None))
            .collect())
    }

    /// An app's top-level windows: its children with a window role, or,
    /// when it has none, those with a size, as Orca lists them.
    async fn app_windows(&self, app: &Node) -> Result<Vec<(Node, WindowMeta)>, CallError> {
        let kids = self.children_raw(app).await?;
        let read = join_all(kids.iter().map(|(d, p)| async move {
            let n = Node::new(d, p, Hint::None);
            let (role, states, name, frame) = tokio::join!(
                self.role(&n),
                self.states(&n),
                self.name(&n),
                self.frame(&n)
            );
            let role = role.ok()?;
            let states = states.unwrap_or_default();
            if states.has(state::DEFUNCT) {
                return None;
            }
            let meta = WindowMeta {
                title: name.unwrap_or_default(),
                frame,
                focused: states.has(state::ACTIVE),
                minimized: states.has(state::ICONIFIED),
            };
            Some((atspi::is_window_role(&role), n, meta))
        }))
        .await;
        let read: Vec<_> = read.into_iter().flatten().collect();
        let standard = read.iter().any(|(is_window, _, _)| *is_window);
        Ok(read
            .into_iter()
            .filter(|(is_window, _, meta)| {
                if standard {
                    *is_window
                } else {
                    meta.frame.is_some()
                }
            })
            .map(|(_, n, meta)| (n, meta))
            .collect())
    }

    /// Searches below `roots`, level by level, for the first element
    /// `want` accepts. Roots themselves are skipped for the focus.
    async fn find(&self, roots: Vec<Node>, want: Want, max_depth: usize) -> Option<Node> {
        let mut frontier = roots;
        let mut seen = 0usize;
        let mut depth = 0usize;
        while !frontier.is_empty() && seen < MAX_NODES && depth <= max_depth {
            frontier.truncate(MAX_NODES - seen);
            let hits = join_all(frontier.iter().map(|n| async move {
                match want {
                    Want::Focused => self.states(n).await.is_ok_and(|s| s.has(state::FOCUSED)),
                    Want::MenuBar => self.role(n).await.is_ok_and(|r| r == "menu bar"),
                }
            }))
            .await;
            let skip_roots = want == Want::Focused && depth == 0;
            if !skip_roots && let Some(i) = hits.iter().position(|h| *h) {
                return Some(frontier[i].clone());
            }
            seen += frontier.len();
            let kids = join_all(frontier.iter().map(|n| self.children_raw(n))).await;
            frontier = kids
                .into_iter()
                .flatten()
                .flatten()
                .map(|(d, p)| Node::new(&d, &p, Hint::None))
                .collect();
            depth += 1;
        }
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Focused,
    MenuBar,
}

/// The program's file name, which an app selector can use.
fn exe_name(pid: i32) -> Option<String> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    let name = exe
        .as_deref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .ok()
                .map(|s| s.trim().to_string())
        })?;
    (!name.is_empty()).then_some(name)
}

impl AtSpi {
    pub fn new() -> Self {
        Self {
            runtime: OnceLock::new(),
            bus: tokio::sync::Mutex::new(None),
            warned: AtomicBool::new(false),
        }
    }

    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("bricolo-atspi")
                .enable_all()
                .build()
                .expect("a Tokio runtime starts")
        })
    }

    /// Runs `f` to the end on this backend's runtime. The engine calls the
    /// backend on blocking threads, where this is allowed.
    fn block<T>(&self, f: impl Future<Output = T>) -> T {
        self.runtime().block_on(f)
    }

    async fn connect(&self) -> Result<Arc<Bus>, CallError> {
        let unavailable = |why: String| {
            warn_once(
                &self.warned,
                &format!(
                    "accessibility: no AT-SPI bus ({why}); install at-spi2-core and run a \
                     desktop session, so accessibility answers unavailable"
                ),
            );
            CallError::Unavailable
        };
        let session = Connection::session()
            .await
            .map_err(|e| unavailable(format!("no D-Bus session bus: {e}")))?;
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            session.call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            ),
        )
        .await
        .map_err(|_| unavailable("org.a11y.Bus did not answer".into()))?
        .map_err(|e| unavailable(format!("org.a11y.Bus: {e}")))?;
        let address: String = reply
            .body()
            .deserialize()
            .map_err(|e| unavailable(e.to_string()))?;
        let conn = zbus::connection::Builder::address(address.as_str())
            .build()
            .await
            .map_err(|e| unavailable(format!("cannot connect to {address}: {e}")))?;
        Ok(Arc::new(Bus {
            conn,
            pids: Mutex::new(HashMap::new()),
            dead: AtomicBool::new(false),
        }))
    }

    async fn bus(&self) -> Result<Arc<Bus>, CallError> {
        let mut slot = self.bus.lock().await;
        if let Some(bus) = &*slot
            && !bus.dead.load(Ordering::Relaxed)
        {
            return Ok(bus.clone());
        }
        let bus = self.connect().await?;
        *slot = Some(bus.clone());
        Ok(bus)
    }

    async fn apps_async(&self) -> Result<Vec<AxApp>, CallError> {
        let bus = self.bus().await?;
        let roots = bus.app_roots().await?;
        let read = join_all(roots.iter().map(|root| {
            let bus = &bus;
            async move {
                let pid = bus.pid(&root.dest).await?;
                let windows = bus.app_windows(root).await.ok()?;
                if windows.is_empty() {
                    return None;
                }
                let name = bus.name(root).await.or_else(|| exe_name(pid))?;
                let frontmost = windows.iter().any(|(_, w)| w.focused);
                Some(AxApp {
                    name,
                    bundle_id: exe_name(pid),
                    pid,
                    frontmost,
                })
            }
        }))
        .await;
        // One app for each process; the windows of the rest join it.
        let mut apps: Vec<AxApp> = Vec::new();
        for app in read.into_iter().flatten() {
            match apps.iter_mut().find(|a| a.pid == app.pid) {
                Some(have) => have.frontmost |= app.frontmost,
                None => apps.push(app),
            }
        }
        Ok(apps)
    }

    /// The registry children that belong to process `pid`.
    async fn roots_of(&self, bus: &Bus, pid: i32) -> Result<Vec<Node>, CallError> {
        let roots = bus.app_roots().await?;
        let pids = join_all(roots.iter().map(|r| bus.pid(&r.dest))).await;
        Ok(roots
            .into_iter()
            .zip(pids)
            .filter(|(_, p)| *p == Some(pid))
            .map(|(r, _)| r)
            .collect())
    }

    async fn windows_async(&self, pid: i32) -> Result<Vec<(Node, WindowMeta)>, CallError> {
        let bus = self.bus().await?;
        let mut out = Vec::new();
        for root in self.roots_of(&bus, pid).await? {
            out.extend(bus.app_windows(&root).await?);
        }
        Ok(out)
    }

    async fn info_async(&self, n: &Node) -> Result<NodeInfo, CallError> {
        let bus = self.bus().await?;
        let (role, states, ifaces, name, description) = tokio::join!(
            bus.role(n),
            bus.states(n),
            bus.interfaces(n),
            bus.name(n),
            bus.description(n)
        );
        let role = role?;
        let states = states.unwrap_or_default();
        if states.has(state::DEFUNCT) {
            return Err(gone());
        }
        let ifaces = ifaces.unwrap_or_default();
        let has = |suffix: &str| ifaces.iter().any(|i| i.ends_with(suffix));
        let kind = atspi::kind(&role, states, n.hint);
        let secure = atspi::is_secure(&role, name.as_deref(), kind.editable());
        // A label's name is its text: skip the second read.
        let reads_text = match kind {
            Kind::TextField | Kind::TextArea | Kind::ComboBox => true,
            Kind::StaticText | Kind::Heading | Kind::Link => name.is_none(),
            _ => false,
        };
        let reads_number = matches!(
            kind,
            Kind::Slider | Kind::ScrollBar | Kind::TextField | Kind::Other
        );
        let (frame, actions, text, number, url) = tokio::join!(
            async {
                if has("Component") {
                    bus.frame(n).await
                } else {
                    None
                }
            },
            async {
                if has("Action") {
                    bus.actions(n).await
                } else {
                    Vec::new()
                }
            },
            async {
                if !secure && reads_text && has("Text") {
                    bus.text_value(n).await
                } else {
                    None
                }
            },
            async {
                if !secure && reads_number && has("Value") {
                    bus.number(n).await
                } else {
                    None
                }
            },
            async {
                if kind == Kind::Link && has("Hyperlink") {
                    bus.uri(n).await
                } else {
                    None
                }
            },
        );
        let mut info = NodeInfo::new(kind, role.clone(), atspi::identity(&n.dest, &n.path));
        info.name = name;
        info.description = description;
        info.url = url;
        info.frame = frame;
        info.actions = atspi::action_names(&actions);
        info.enabled = states.usable();
        info.focused = states.has(state::FOCUSED);
        info.selected = states.has(state::SELECTED);
        if states.has(state::EXPANDABLE) {
            info.expanded = Some(states.has(state::EXPANDED));
        }
        if !secure {
            info.value = text.or_else(|| number.map(atspi::number_text));
            if atspi::is_checkable_role(&role) {
                let on = states.has(state::CHECKED);
                if matches!(kind, Kind::CheckBox | Kind::RadioButton) {
                    info.value = Some(if on { "1" } else { "0" }.into());
                } else {
                    info.selected |= on;
                }
            }
        }
        Ok(info)
    }

    async fn children_async(&self, n: &Node) -> Result<Vec<Node>, CallError> {
        let bus = self.bus().await?;
        let (kids, role) = tokio::join!(bus.children_raw(n), bus.role(n));
        let hint = atspi::hint_for_children(role.as_deref().unwrap_or(""));
        Ok(kids?.iter().map(|(d, p)| Node::new(d, p, hint)).collect())
    }

    /// Windows of `pid`, the active one first.
    async fn window_nodes(&self, pid: i32) -> Vec<Node> {
        let mut windows = self.windows_async(pid).await.unwrap_or_default();
        windows.sort_by_key(|(_, w)| !w.focused);
        windows.into_iter().map(|(n, _)| n).collect()
    }

    async fn focused_async(&self, pid: i32) -> Option<Node> {
        let bus = self.bus().await.ok()?;
        let roots = self.window_nodes(pid).await;
        bus.find(roots, Want::Focused, FOCUS_DEPTH).await
    }

    async fn menu_bar_async(&self, pid: i32) -> Option<Node> {
        let bus = self.bus().await.ok()?;
        let roots = self.window_nodes(pid).await;
        bus.find(roots, Want::MenuBar, MENU_BAR_DEPTH).await
    }

    async fn press_async(&self, n: &Node) -> Result<(), CallError> {
        let bus = self.bus().await?;
        let actions = bus.actions(n).await;
        if let Some(i) = atspi::preferred_action(&actions) {
            return bus.do_action(n, i).await;
        }
        // A list or table row with no press action is picked through its
        // container's Selection, as Orca does.
        if let Some(()) = self.select_in_parent(&bus, n).await {
            return Ok(());
        }
        Err(CallError::Failed(
            "element_not_clickable: the element has no press action; try its parent or a listed action"
                .into(),
        ))
    }

    async fn select_in_parent(&self, bus: &Bus, n: &Node) -> Option<()> {
        let index: i32 = bus.on(n, ACCESSIBLE, "GetIndexInParent", &()).await.ok()?;
        if index < 0 {
            return None;
        }
        let parent: OwnedVariant = bus
            .on(n, PROPERTIES, "Get", &(ACCESSIBLE, "Parent"))
            .await
            .ok()?;
        let (dest, path): (String, OwnedObjectPath) = parent.try_into().ok()?;
        if dest.is_empty() || path.as_str() == NULL_PATH {
            return None;
        }
        let parent = Node::new(&dest, path.as_str(), Hint::None);
        let done: bool = bus
            .on(&parent, SELECTION, "SelectChild", &(index,))
            .await
            .ok()?;
        done.then_some(())
    }

    async fn perform_async(&self, n: &Node, action: &str) -> Result<(), CallError> {
        let bus = self.bus().await?;
        let actions = bus.actions(n).await;
        let i = atspi::action_index(&actions, action)
            .ok_or_else(|| CallError::Failed(format!("action_not_supported: {action}")))?;
        bus.do_action(n, i).await
    }

    async fn set_value_async(&self, n: &Node, value: &str) -> Result<(), CallError> {
        let bus = self.bus().await?;
        let (role, states, ifaces) = tokio::join!(bus.role(n), bus.states(n), bus.interfaces(n));
        let role = role?;
        let states = states.unwrap_or_default();
        let ifaces = ifaces.unwrap_or_default();
        let has = |suffix: &str| ifaces.iter().any(|i| i.ends_with(suffix));
        if atspi::is_checkable_role(&role) && has("Action") {
            // A switch is set by pressing it when it differs.
            if atspi::truthy(value) != states.has(state::CHECKED) {
                self.press_async(n).await?;
            }
            return Ok(());
        }
        if has("EditableText") {
            let done: bool = bus
                .on(n, EDITABLE_TEXT, "SetTextContents", &(value,))
                .await
                .unwrap_or(false);
            if done {
                return Ok(());
            }
        }
        if has("Value") {
            let number: f64 = value.trim().parse().map_err(|_| {
                CallError::Failed(format!("invalid_argument: {value:?} is not a number"))
            })?;
            let _: () = bus
                .on(
                    n,
                    PROPERTIES,
                    "Set",
                    &(VALUE, "CurrentValue", Variant::F64(number)),
                )
                .await?;
            return Ok(());
        }
        Err(CallError::Failed(
            "value_not_settable: the element's value cannot be set; click it or type into it"
                .into(),
        ))
    }

    async fn insert_text_async(&self, n: &Node, text: &str) -> Result<(), CallError> {
        let bus = self.bus().await?;
        let (states, ifaces) = tokio::join!(bus.states(n), bus.interfaces(n));
        let ifaces = ifaces.unwrap_or_default();
        let has = |suffix: &str| ifaces.iter().any(|i| i.ends_with(suffix));
        let not_settable = || {
            CallError::Failed(
                "value_not_settable: the element does not take text; click it or type into it"
                    .into(),
            )
        };
        if !has("EditableText") || !has("Text") || !states.unwrap_or_default().has(state::EDITABLE)
        {
            return Err(not_settable());
        }
        // The selection, or the caret, or the end.
        let count: i32 = bus.get(n, TEXT, "CharacterCount").await.unwrap_or(0);
        let caret: i32 = bus.get(n, TEXT, "CaretOffset").await.unwrap_or(-1);
        let selections: i32 = bus.on(n, TEXT, "GetNSelections", &()).await.unwrap_or(0);
        let (mut start, mut end) = if caret >= 0 {
            (caret, caret)
        } else {
            (count, count)
        };
        if selections == 1
            && let Ok((s, e)) = bus
                .on::<_, (i32, i32)>(n, TEXT, "GetSelection", &(0i32,))
                .await
            && 0 <= s
            && s < e
            && e <= count
        {
            (start, end) = (s, e);
        }
        // The length is in bytes, the positions in characters.
        let bytes = i32::try_from(text.len()).map_err(|_| not_settable())?;
        // Insert before deleting: a refused insert leaves the selection.
        let done: bool = bus
            .on(n, EDITABLE_TEXT, "InsertText", &(end, text, bytes))
            .await?;
        if !done {
            return Err(not_settable());
        }
        if end > start {
            let _: bool = bus
                .on(n, EDITABLE_TEXT, "DeleteText", &(start, end))
                .await
                .unwrap_or(false);
        }
        let chars = i32::try_from(text.chars().count()).unwrap_or(0);
        let _: Result<bool, _> = bus.on(n, TEXT, "SetCaretOffset", &(start + chars,)).await;
        Ok(())
    }
}

impl AxBackend for AtSpi {
    type Node = Node;

    fn apps(&self) -> Result<Vec<AxApp>, CallError> {
        self.block(self.apps_async())
    }

    fn windows(&self, pid: i32) -> Result<Vec<(Node, WindowMeta)>, CallError> {
        self.block(self.windows_async(pid))
    }

    fn info(&self, node: &Node) -> Result<NodeInfo, CallError> {
        self.block(self.info_async(node))
    }

    fn children(&self, node: &Node) -> Result<Vec<Node>, CallError> {
        self.block(self.children_async(node))
    }

    fn focused(&self, pid: i32) -> Option<Node> {
        self.block(self.focused_async(pid))
    }

    fn menu_bar(&self, pid: i32) -> Option<Node> {
        self.block(self.menu_bar_async(pid))
    }

    fn press(&self, node: &Node) -> Result<(), CallError> {
        self.block(self.press_async(node))
    }

    fn perform(&self, node: &Node, action: &str) -> Result<(), CallError> {
        self.block(self.perform_async(node, action))
    }

    fn set_value(&self, node: &Node, value: &str) -> Result<(), CallError> {
        self.block(self.set_value_async(node, value))
    }

    fn insert_text(&self, node: &Node, text: &str) -> Result<(), CallError> {
        self.block(self.insert_text_async(node, text))
    }

    fn press_key(&self, pid: i32, chord: &Chord) -> Result<(), CallError> {
        match Session::current() {
            Session::X11 => {}
            Session::Wayland => {
                return Err(CallError::Failed(
                    "unsupported_capability: press-key on Wayland: native Wayland windows ignore \
                     synthetic keys, so nothing was sent; use type-text or set-value with an \
                     element"
                        .into(),
                ));
            }
            Session::Headless => return Err(CallError::Unavailable),
        }
        // XTEST keys go to whatever window has the focus.
        let active = self.windows(pid)?.iter().any(|(_, w)| w.focused);
        if !active {
            return Err(CallError::Failed(
                "background_unavailable: press-key sends keys to the focused window, and the \
                 app's window is not the active one; nothing was sent. To enter text use \
                 type-text or set-value with an element, which work in the background"
                    .into(),
            ));
        }
        super::keyboard::press_chord(chord)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::{Accessibility, AccessibilityProvider, Verification};
    use crate::caller::Caller;

    #[test]
    fn nodes_are_cheap_to_clone_and_keep_their_name() {
        let n = Node::new(":1.7", "/org/a11y/atspi/accessible/42", Hint::None);
        let c = n.clone();
        assert_eq!(&*c.dest, ":1.7");
        assert_eq!(&*c.path, "/org/a11y/atspi/accessible/42");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn block_runs_on_the_blocking_pool_of_another_runtime() {
        let at = Arc::new(AtSpi::new());
        let a = at.clone();
        let n = tokio::task::spawn_blocking(move || {
            a.block(async {
                tokio::task::yield_now().await;
                7
            })
        })
        .await
        .unwrap();
        assert_eq!(n, 7);
        // Dropping inside async code must not panic.
        drop(at);
    }

    #[test]
    fn errors_for_vanished_elements_say_element_not_found() {
        let e = map_err(&zbus::Error::FDO(Box::new(
            zbus::fdo::Error::UnknownObject("x".into()),
        )));
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("element_not_found")));
        let e = map_err(&zbus::Error::FDO(Box::new(zbus::fdo::Error::NoReply(
            "x".into(),
        ))));
        assert!(matches!(e, CallError::Failed(m) if m.starts_with("accessibility_error")));
    }

    fn xterm_free_program(name: &str) -> Option<String> {
        super::super::which(name).map(|p| p.display().to_string())
    }

    /// Needs an X server, a D-Bus session with at-spi2-core and zenity, as
    /// `just linux-test` provides: opens a GTK entry dialog, reads its
    /// tree, sets the entry and presses OK, then checks what zenity printed.
    #[ignore = "needs an X server, the AT-SPI bus and zenity; run by just linux-test"]
    #[tokio::test(flavor = "multi_thread")]
    async fn x11_reads_and_drives_a_gtk_dialog() {
        let Some(zenity) = xterm_free_program("zenity") else {
            eprintln!("zenity is not installed; skipping");
            return;
        };
        let mut child = std::process::Command::new(zenity)
            .args(["--entry", "--title=bricolo-atspi", "--text=Name:"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ax = Accessibility::new(AtSpi::new(), None);
        let c = Caller::new("test");
        let app = format!("pid:{}", child.id());
        let found = ax
            .wait(&c, app.clone(), "OK".into(), Some(20_000))
            .await
            .unwrap_or_else(|e| {
                let _ = child.kill();
                panic!("zenity never showed an OK element: {e:?}")
            });
        assert!(found.iter().any(|e| e.role.contains("button")), "{found:?}");
        let state = ax
            .snapshot(&c, app.clone(), None, None, None, None)
            .await
            .unwrap();
        eprintln!("{}", state.tree);
        assert!(state.element_count > 3);
        let apps = ax.list_apps(&c).await.unwrap();
        assert!(apps.iter().any(|a| a.pid as u32 == child.id()), "{apps:?}");
        // The entry, then OK.
        let all = ax.find(&c, app.clone(), String::new()).await.unwrap();
        let entry = all
            .iter()
            .find(|e| matches!(e.role.as_str(), "entry" | "text") && e.enabled)
            .unwrap_or_else(|| panic!("no entry in {all:?}"));
        let set = ax
            .set_value(&c, app.clone(), entry.index, "hello bricolo".into())
            .await
            .unwrap();
        assert_eq!(set.verification, Verification::Verified, "{set:?}");
        let ok = all
            .iter()
            .find(|e| e.role == "push button" && e.name.as_deref() == Some("OK"))
            .unwrap_or_else(|| panic!("no OK button in {all:?}"));
        let _ = ax.click(&c, app, ok.index).await;
        let out = tokio::task::spawn_blocking(move || child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello bricolo");
    }
}
