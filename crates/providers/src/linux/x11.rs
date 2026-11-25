//! One X11 connection for `window` and `app`, opened on first use and
//! again after the server goes away. Calls block, so they run on Tokio's
//! blocking pool. Everything here follows EWMH: the window manager owns the
//! window list, the active window and the frames.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use x11rb::connection::Connection;
use x11rb::errors::{ConnectionError, ReplyError};
use x11rb::protocol::ErrorKind;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask,
    InputFocus, StackMode, Window,
};
use x11rb::rust_connection::RustConnection;

use super::ewmh::{self, Extents};
use super::{Session, warn_once};
use crate::error::CallError;
use crate::window::{Rect, Screen, WindowInfo};

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        UTF8_STRING,
        _NET_SUPPORTED,
        _NET_CLIENT_LIST,
        _NET_ACTIVE_WINDOW,
        _NET_CLOSE_WINDOW,
        WM_CHANGE_STATE,
        _NET_CURRENT_DESKTOP,
        _NET_WORKAREA,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_FRAME_EXTENTS,
        _NET_MOVERESIZE_WINDOW,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DESKTOP,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_TOOLBAR,
        _NET_WM_WINDOW_TYPE_MENU,
        _NET_WM_WINDOW_TYPE_SPLASH,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
    }
}

pub enum XError {
    /// The connection broke: the next call connects again.
    Lost(String),
    /// The window went away, or never was a managed client.
    NotFound,
    Failed(String),
}

impl From<ConnectionError> for XError {
    fn from(e: ConnectionError) -> Self {
        Self::Lost(e.to_string())
    }
}

impl From<ReplyError> for XError {
    fn from(e: ReplyError) -> Self {
        match e {
            ReplyError::ConnectionError(e) => e.into(),
            ReplyError::X11Error(e) if e.error_kind == ErrorKind::Window => Self::NotFound,
            ReplyError::X11Error(e) => Self::Failed(format!("X11 error {:?}", e.error_kind)),
        }
    }
}

impl From<XError> for CallError {
    fn from(e: XError) -> Self {
        match e {
            XError::Lost(msg) => CallError::Failed(format!("lost the X server: {msg}")),
            XError::NotFound => CallError::Failed("no such window".into()),
            XError::Failed(msg) => CallError::Failed(msg),
        }
    }
}

/// An open connection and its atoms.
pub struct Conn {
    conn: RustConnection,
    screen: usize,
    root: Window,
    atoms: Atoms,
}

/// The shared, lazily opened connection.
#[derive(Default)]
pub struct X11 {
    conn: Mutex<Option<Arc<Conn>>>,
    warned: AtomicBool,
}

impl X11 {
    /// Opens the connection if needed. Answers `unavailable` on Wayland
    /// and without a display, and logs why the first time.
    fn get(&self) -> Result<Arc<Conn>, CallError> {
        match Session::current() {
            Session::X11 => {}
            Session::Wayland => {
                warn_once(
                    &self.warned,
                    "window, hotkey and the app list need X11 or Hyprland; this is another Wayland session, \
                     so they answer unavailable (see docs/linux.md)",
                );
                return Err(CallError::Unavailable);
            }
            Session::Headless => {
                warn_once(
                    &self.warned,
                    "no DISPLAY: window, hotkey and the app list answer unavailable",
                );
                return Err(CallError::Unavailable);
            }
        }
        let mut slot = self.conn.lock().unwrap();
        if let Some(conn) = &*slot {
            return Ok(conn.clone());
        }
        let conn = Conn::open().map_err(|e| {
            warn_once(&self.warned, &format!("cannot open the X display: {e}"));
            tracing::debug!("cannot open the X display: {e}");
            CallError::Unavailable
        })?;
        let conn = Arc::new(conn);
        *slot = Some(conn.clone());
        Ok(conn)
    }

    /// Runs `f` on the blocking pool with the connection.
    pub async fn with<T: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Conn) -> Result<T, XError> + Send + 'static,
    ) -> Result<T, CallError> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = this.get()?;
            let result = f(&conn);
            if let Err(XError::Lost(_)) = &result {
                this.conn.lock().unwrap().take();
            }
            result.map_err(CallError::from)
        })
        .await
        .map_err(CallError::failed)?
    }
}

impl Conn {
    fn open() -> Result<Self, String> {
        let (conn, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms::new(&conn)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            conn,
            screen,
            root,
            atoms,
        })
    }

    fn property(
        &self,
        window: Window,
        property: Atom,
        kind: impl Into<Atom>,
    ) -> Result<Vec<u8>, XError> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 1 << 20)?
            .reply()?;
        Ok(reply.value)
    }

    fn cardinals(
        &self,
        window: Window,
        property: Atom,
        kind: AtomEnum,
    ) -> Result<Vec<u32>, XError> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 1 << 16)?
            .reply()?;
        Ok(reply.value32().map(Iterator::collect).unwrap_or_default())
    }

    fn supports(&self, atom: Atom) -> Result<bool, XError> {
        let supported = self.cardinals(self.root, self.atoms._NET_SUPPORTED, AtomEnum::ATOM)?;
        Ok(supported.contains(&atom))
    }

    /// The managed windows, oldest first.
    pub fn clients(&self) -> Result<Vec<Window>, XError> {
        self.cardinals(self.root, self.atoms._NET_CLIENT_LIST, AtomEnum::WINDOW)
    }

    /// The window `id`, if the window manager manages it. Plugins can only
    /// act on managed windows, never on the root or a panel's internals.
    pub fn client(&self, id: u64) -> Result<Window, XError> {
        let window = Window::try_from(id).map_err(|_| XError::NotFound)?;
        if self.clients()?.contains(&window) {
            Ok(window)
        } else {
            Err(XError::NotFound)
        }
    }

    pub fn active(&self) -> Result<Option<Window>, XError> {
        let active = self.cardinals(self.root, self.atoms._NET_ACTIVE_WINDOW, AtomEnum::WINDOW)?;
        Ok(active.first().copied().filter(|&w| w != 0))
    }

    /// Whether the window is one a user switches to: not a panel, dock,
    /// desktop, menu, splash screen or notification.
    pub fn is_app_window(&self, window: Window) -> Result<bool, XError> {
        let a = &self.atoms;
        let skip = [
            a._NET_WM_WINDOW_TYPE_DESKTOP,
            a._NET_WM_WINDOW_TYPE_DOCK,
            a._NET_WM_WINDOW_TYPE_TOOLBAR,
            a._NET_WM_WINDOW_TYPE_MENU,
            a._NET_WM_WINDOW_TYPE_SPLASH,
            a._NET_WM_WINDOW_TYPE_NOTIFICATION,
        ];
        let types = self.cardinals(window, a._NET_WM_WINDOW_TYPE, AtomEnum::ATOM)?;
        Ok(!types.iter().any(|t| skip.contains(t)))
    }

    /// `WM_CLASS` as (instance, class).
    pub fn class(&self, window: Window) -> Result<(String, String), XError> {
        let value = self.property(window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING)?;
        Ok(ewmh::wm_class(&value))
    }

    pub fn pid(&self, window: Window) -> Result<Option<u32>, XError> {
        let pid = self.cardinals(window, self.atoms._NET_WM_PID, AtomEnum::CARDINAL)?;
        Ok(pid.first().copied())
    }

    fn title(&self, window: Window) -> Result<String, XError> {
        let name = self.property(window, self.atoms._NET_WM_NAME, self.atoms.UTF8_STRING)?;
        if !name.is_empty() {
            return Ok(ewmh::text(&name));
        }
        let name = self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::ANY)?;
        Ok(ewmh::text(&name))
    }

    fn extents(&self, window: Window) -> Result<Extents, XError> {
        let values = self.cardinals(window, self.atoms._NET_FRAME_EXTENTS, AtomEnum::CARDINAL)?;
        Ok(Extents::from_values(&values))
    }

    /// The outer frame, decorations included, in root coordinates.
    fn frame(&self, window: Window) -> Result<Rect, XError> {
        let geometry = self.conn.get_geometry(window)?.reply()?;
        let at = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)?
            .reply()?;
        Ok(self.extents(window)?.outer(
            at.dst_x.into(),
            at.dst_y.into(),
            geometry.width.into(),
            geometry.height.into(),
        ))
    }

    pub fn info(&self, window: Window) -> Result<WindowInfo, XError> {
        Ok(WindowInfo {
            id: window.into(),
            app: self.class(window)?.1,
            title: self.title(window)?,
            frame: self.frame(window)?,
        })
    }

    fn send(&self, window: Window, kind: Atom, data: [u32; 5]) -> Result<(), XError> {
        let event = ClientMessageEvent::new(32, window, kind, data);
        self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )?;
        Ok(())
    }

    /// Asks the window manager to activate the window: it switches desktop
    /// if needed, raises it and gives it focus. Without EWMH, raises the
    /// window and sets the input focus directly.
    pub fn focus(&self, window: Window) -> Result<(), XError> {
        if self.supports(self.atoms._NET_ACTIVE_WINDOW)? {
            self.send(window, self.atoms._NET_ACTIVE_WINDOW, ewmh::activate_data())?;
        } else {
            let raise = ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
            self.conn.configure_window(window, &raise)?.check()?;
            self.conn
                .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)?
                .check()?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Asks the window manager to close the window, as its close button
    /// does.
    pub fn close(&self, window: Window) -> Result<(), XError> {
        self.send(window, self.atoms._NET_CLOSE_WINDOW, ewmh::close_data())?;
        self.conn.flush()?;
        Ok(())
    }

    /// Iconifies the window, or activates it again to restore it.
    pub fn set_minimized(&self, window: Window, minimized: bool) -> Result<(), XError> {
        if minimized {
            self.send(
                window,
                self.atoms.WM_CHANGE_STATE,
                [ewmh::ICONIC_STATE, 0, 0, 0, 0],
            )?;
            self.conn.flush()?;
            Ok(())
        } else {
            self.focus(window)
        }
    }

    /// Moves and resizes the outer frame to `frame`. A maximized window is
    /// restored first, since window managers keep maximized windows put.
    pub fn set_frame(&self, window: Window, frame: Rect) -> Result<(), XError> {
        let a = &self.atoms;
        let extents = self.extents(window)?;
        let (width, height) = extents.inner_size(frame);
        let unmaximize = [
            ewmh::STATE_REMOVE,
            a._NET_WM_STATE_MAXIMIZED_VERT,
            a._NET_WM_STATE_MAXIMIZED_HORZ,
            ewmh::SOURCE_PAGER,
            0,
        ];
        self.send(window, a._NET_WM_STATE, unmaximize)?;
        if self.supports(a._NET_MOVERESIZE_WINDOW)? {
            let data = ewmh::moveresize_data(frame, width, height);
            self.send(window, a._NET_MOVERESIZE_WINDOW, data)?;
        } else {
            let to = ConfigureWindowAux::new()
                .x(frame.x + extents.left as i32)
                .y(frame.y + extents.top as i32)
                .width(width)
                .height(height);
            self.conn.configure_window(window, &to)?.check()?;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// The monitors from RandR 1.5, or the whole root window without it.
    /// The visible frame leaves out panels, from `_NET_WORKAREA`.
    pub fn screens(&self) -> Result<Vec<Screen>, XError> {
        let a = &self.atoms;
        let desktop = self.cardinals(self.root, a._NET_CURRENT_DESKTOP, AtomEnum::CARDINAL)?;
        let areas = self.cardinals(self.root, a._NET_WORKAREA, AtomEnum::CARDINAL)?;
        let area = ewmh::workarea(&areas, desktop.first().copied().unwrap_or(0));
        let monitors = match self.conn.randr_get_monitors(self.root, true) {
            Ok(cookie) => cookie.reply()?.monitors,
            Err(ConnectionError::UnsupportedExtension) => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let mut screens: Vec<Screen> = monitors
            .iter()
            .map(|m| {
                let frame = Rect {
                    x: m.x.into(),
                    y: m.y.into(),
                    width: m.width.into(),
                    height: m.height.into(),
                };
                Screen {
                    id: m.name,
                    frame,
                    visible_frame: ewmh::visible_frame(frame, area),
                    main: m.primary,
                }
            })
            .collect();
        if screens.is_empty() {
            let root = &self.conn.setup().roots[self.screen];
            let frame = Rect {
                x: 0,
                y: 0,
                width: root.width_in_pixels.into(),
                height: root.height_in_pixels.into(),
            };
            screens.push(Screen {
                id: 0,
                frame,
                visible_frame: ewmh::visible_frame(frame, area),
                main: true,
            });
        }
        if !screens.iter().any(|s| s.main) {
            screens[0].main = true;
        }
        Ok(screens)
    }
}
