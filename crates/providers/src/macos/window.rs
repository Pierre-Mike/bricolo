//! `window` on the macOS Accessibility API. AX calls can block on an app
//! that does not answer, so they run on Tokio's blocking pool. Screens come
//! from NSScreen, which only works on the main-thread loop.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString, CFType, CGPoint, CGSize};

use super::main_loop::MainHandle;
use crate::caller::Caller;
use crate::error::CallError;
use crate::window::{Rect, Screen, WindowInfo, WindowProvider};

unsafe extern "C" {
    /// Private, but stable for years: Hammerspoon, yabai and AeroSpace use it.
    fn _AXUIElementGetWindow(element: &AXUIElement, id: *mut u32) -> AXError;
}

/// An AX element. CF objects retain and release atomically, and the cache
/// is only used under its lock.
struct Element(CFRetained<AXUIElement>);

// SAFETY: see above; AXUIElement calls are thread-safe.
unsafe impl Send for Element {}

struct Cached {
    pid: i32,
    window: Element,
}

#[derive(Default)]
struct Ax {
    /// Window id to its element. Refilled by `list` on a miss.
    cache: HashMap<u64, Cached>,
}

pub(super) enum AxErr {
    NotFound,
    Denied,
    Failed(String),
}

impl From<AxErr> for CallError {
    fn from(e: AxErr) -> Self {
        match e {
            AxErr::NotFound => CallError::Failed("no such window".into()),
            AxErr::Denied => CallError::Denied,
            AxErr::Failed(msg) => CallError::Failed(msg),
        }
    }
}

pub struct MacWindows {
    ax: Arc<Mutex<Ax>>,
    main: Option<MainHandle>,
}

impl MacWindows {
    /// Without `main`, `screens` answers `unavailable`.
    pub fn new(main: Option<MainHandle>) -> Self {
        Self {
            ax: Arc::default(),
            main,
        }
    }

    async fn with_ax<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Ax) -> Result<T, AxErr> + Send + 'static,
    ) -> Result<T, CallError> {
        let ax = self.ax.clone();
        tokio::task::spawn_blocking(move || f(&mut ax.lock().unwrap()))
            .await
            .map_err(CallError::failed)?
            .map_err(CallError::from)
    }
}

#[async_trait]
impl WindowProvider for MacWindows {
    async fn list(&self, _: &Caller) -> Result<Vec<WindowInfo>, CallError> {
        self.with_ax(|ax| ax.list()).await
    }

    async fn focused(&self, _: &Caller) -> Result<Option<WindowInfo>, CallError> {
        self.with_ax(|ax| ax.focused()).await
    }

    async fn focus(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        self.with_ax(move |ax| ax.focus(id)).await
    }

    async fn set_frame(&self, _: &Caller, id: u64, frame: Rect) -> Result<(), CallError> {
        self.with_ax(move |ax| ax.set_frame(id, frame)).await
    }

    async fn close(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        self.with_ax(move |ax| ax.close(id)).await
    }

    async fn set_minimized(&self, _: &Caller, id: u64, minimized: bool) -> Result<(), CallError> {
        self.with_ax(move |ax| ax.set_minimized(id, minimized))
            .await
    }

    async fn screens(&self, _: &Caller) -> Result<Vec<Screen>, CallError> {
        let main = self.main.as_ref().ok_or(CallError::Unavailable)?;
        main.screens().await.map_err(CallError::Failed)
    }
}

impl Ax {
    fn focused(&mut self) -> Result<Option<WindowInfo>, AxErr> {
        let system = unsafe { AXUIElement::new_system_wide() };
        let app = match element_attr(&system, "AXFocusedApplication")? {
            Some(app) => app,
            // The system-wide element can have no answer: use the frontmost app.
            None => {
                let Some(pid) = frontmost_pid() else {
                    return Ok(None);
                };
                unsafe { AXUIElement::new_application(pid) }
            }
        };
        let window = match element_attr(&app, "AXFocusedWindow")? {
            Some(window) => window,
            None => match element_attr(&app, "AXMainWindow")? {
                Some(window) => window,
                None => return Ok(None),
            },
        };
        let mut pid = 0;
        check(unsafe { app.pid(NonNull::from(&mut pid)) })?;
        let app_name = string_attr(&app, "AXTitle")?.unwrap_or_default();
        let info = window_info(&window, &app_name)?;
        self.cache.insert(
            info.id,
            Cached {
                pid,
                window: Element(window),
            },
        );
        Ok(Some(info))
    }

    fn list(&mut self) -> Result<Vec<WindowInfo>, AxErr> {
        let mut out = Vec::new();
        self.cache.clear();
        let mut denied = false;
        for pid in regular_app_pids() {
            let app = unsafe { AXUIElement::new_application(pid) };
            let app_name = match string_attr(&app, "AXTitle") {
                Ok(name) => name.unwrap_or_default(),
                Err(AxErr::Denied) => {
                    denied = true;
                    break;
                }
                Err(_) => continue,
            };
            // Apps without windows, or that refuse AX, are skipped.
            let Ok(windows) = elements_attr(&app, "AXWindows") else {
                continue;
            };
            for window in windows {
                if let Ok(info) = window_info(&window, &app_name) {
                    self.cache.insert(
                        info.id,
                        Cached {
                            pid,
                            window: Element(window),
                        },
                    );
                    out.push(info);
                }
            }
        }
        if denied && !super::accessibility_granted() {
            return Err(AxErr::Denied);
        }
        Ok(out)
    }

    fn cached(&mut self, id: u64) -> Result<&Cached, AxErr> {
        if !self.cache.contains_key(&id) {
            self.list()?;
        }
        self.cache.get(&id).ok_or(AxErr::NotFound)
    }

    fn focus(&mut self, id: u64) -> Result<(), AxErr> {
        let cached = self.cached(id)?;
        let window = &cached.window.0;
        let app = unsafe { AXUIElement::new_application(cached.pid) };
        let yes = CFBoolean::new(true);
        // Frontmost first, then raise: raising alone leaves the app behind.
        check(unsafe { app.set_attribute_value(&CFString::from_str("AXFrontmost"), yes) })?;
        let _ = unsafe { window.set_attribute_value(&CFString::from_str("AXMain"), yes) };
        check(unsafe { window.perform_action(&CFString::from_str("AXRaise")) })
    }

    fn close(&mut self, id: u64) -> Result<(), AxErr> {
        let window = &self.cached(id)?.window.0;
        let button = element_attr(window, "AXCloseButton")?
            .ok_or_else(|| AxErr::Failed("the window has no close button".into()))?;
        check(unsafe { button.perform_action(&CFString::from_str("AXPress")) })
    }

    fn set_minimized(&mut self, id: u64, minimized: bool) -> Result<(), AxErr> {
        let window = &self.cached(id)?.window.0;
        let value = CFBoolean::new(minimized);
        check(unsafe { window.set_attribute_value(&CFString::from_str("AXMinimized"), value) })?;
        if minimized {
            return Ok(());
        }
        // A restored window is back but not focused: raise it like `focus`.
        self.focus(id)
    }

    fn set_frame(&mut self, id: u64, frame: Rect) -> Result<(), AxErr> {
        let window = &self.cached(id)?.window.0;
        let size = CGSize::new(frame.width as f64, frame.height as f64);
        let origin = CGPoint::new(frame.x as f64, frame.y as f64);
        // Size, position, size: a window moved to another screen can be
        // clamped to the old screen's size by the first resize.
        set_size(window, size)?;
        set_position(window, origin)?;
        set_size(window, size)
    }
}

pub(super) fn check(err: AXError) -> Result<(), AxErr> {
    match err {
        AXError::Success => Ok(()),
        AXError::APIDisabled | AXError::NotImplemented => Err(AxErr::Denied),
        // An untrusted process gets "cannot complete" rather than "disabled".
        AXError::CannotComplete if !super::accessibility_granted() => Err(AxErr::Denied),
        AXError::InvalidUIElement | AXError::NoValue | AXError::AttributeUnsupported => {
            Err(AxErr::NotFound)
        }
        other => Err(AxErr::Failed(format!("AXError {}", other.0))),
    }
}

/// Reads an attribute. `Ok(None)` when the element has no value for it.
pub(super) fn attr(element: &AXUIElement, name: &str) -> Result<Option<CFRetained<CFType>>, AxErr> {
    let name = CFString::from_str(name);
    let mut value: *const CFType = std::ptr::null();
    let err = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut value)) };
    match check(err) {
        Ok(()) => {
            Ok(NonNull::new(value as *mut CFType).map(|ptr| unsafe { CFRetained::from_raw(ptr) }))
        }
        Err(AxErr::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

pub(super) fn element_attr(
    element: &AXUIElement,
    name: &str,
) -> Result<Option<CFRetained<AXUIElement>>, AxErr> {
    Ok(attr(element, name)?.and_then(|v| v.downcast::<AXUIElement>().ok()))
}

pub(super) fn string_attr(element: &AXUIElement, name: &str) -> Result<Option<String>, AxErr> {
    Ok(attr(element, name)?
        .and_then(|v| v.downcast::<CFString>().ok())
        .map(|s| s.to_string()))
}

pub(super) fn elements_attr(
    element: &AXUIElement,
    name: &str,
) -> Result<Vec<CFRetained<AXUIElement>>, AxErr> {
    let Some(value) = attr(element, name)? else {
        return Ok(Vec::new());
    };
    let array = value
        .downcast::<CFArray>()
        .map_err(|_| AxErr::Failed(format!("{name} is not an array")))?;
    let array: CFRetained<CFArray<AXUIElement>> = unsafe { CFRetained::cast_unchecked(array) };
    Ok(array.iter().collect())
}

pub(super) fn ax_value<T>(
    element: &AXUIElement,
    name: &str,
    kind: AXValueType,
    out: &mut T,
) -> Result<(), AxErr> {
    let value = attr(element, name)?
        .and_then(|v| v.downcast::<AXValue>().ok())
        .ok_or(AxErr::NotFound)?;
    let ok = unsafe { value.value(kind, NonNull::from(out).cast::<c_void>()) };
    if ok {
        Ok(())
    } else {
        Err(AxErr::Failed(format!("{name} has an unexpected type")))
    }
}

fn window_info(window: &AXUIElement, app_name: &str) -> Result<WindowInfo, AxErr> {
    let mut id: u32 = 0;
    check(unsafe { _AXUIElementGetWindow(window, &mut id) })?;
    let mut origin = CGPoint::new(0.0, 0.0);
    let mut size = CGSize::new(0.0, 0.0);
    ax_value(window, "AXPosition", AXValueType::CGPoint, &mut origin)?;
    ax_value(window, "AXSize", AXValueType::CGSize, &mut size)?;
    Ok(WindowInfo {
        id: id as u64,
        app: app_name.to_string(),
        title: string_attr(window, "AXTitle")?.unwrap_or_default(),
        frame: Rect {
            x: origin.x.round() as i32,
            y: origin.y.round() as i32,
            width: size.width.max(0.0).round() as u32,
            height: size.height.max(0.0).round() as u32,
        },
    })
}

fn set_size(window: &AXUIElement, mut size: CGSize) -> Result<(), AxErr> {
    let value = unsafe { AXValue::new(AXValueType::CGSize, NonNull::from(&mut size).cast()) }
        .ok_or_else(|| AxErr::Failed("AXValueCreate failed".into()))?;
    let name = CFString::from_str("AXSize");
    check(unsafe { window.set_attribute_value(&name, &value) })
}

fn set_position(window: &AXUIElement, mut origin: CGPoint) -> Result<(), AxErr> {
    let value = unsafe { AXValue::new(AXValueType::CGPoint, NonNull::from(&mut origin).cast()) }
        .ok_or_else(|| AxErr::Failed("AXValueCreate failed".into()))?;
    let name = CFString::from_str("AXPosition");
    check(unsafe { window.set_attribute_value(&name, &value) })
}

fn frontmost_pid() -> Option<i32> {
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|app| app.processIdentifier())
            .filter(|pid| *pid > 0)
    })
}

/// Pids of apps that show in the Dock, the ones that own normal windows.
fn regular_app_pids() -> Vec<i32> {
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .runningApplications()
            .iter()
            .filter(|app| app.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .map(|app| app.processIdentifier())
            .filter(|pid| *pid > 0)
            .collect()
    })
}
