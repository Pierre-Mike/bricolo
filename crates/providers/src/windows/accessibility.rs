//! `accessibility` on UI Automation: the backend under
//! [`crate::accessibility::Accessibility`]. Apps are the processes that
//! own an Alt+Tab window, windows are those windows' UIA elements, and the
//! tree is read with the control-view `TreeWalker`. Actions go through UIA
//! patterns (invoke, toggle, select, expand and collapse, value, range
//! value), so nothing moves the pointer or raises a window; keys are the
//! one exception and go only to the window in front, through `SendInput`.
//!
//! Identity is the element's runtime id, hashed. Elements are shared
//! across threads: the UIA client objects live in the multithreaded
//! apartment, where every call is marshalled, and each thread that calls
//! in joins that apartment once (`ensure_com`).
//!
//! Text fields get three paths. A `ValuePattern` is the clean one. Edit
//! and RichEdit controls (Notepad's document among them, which has no
//! `ValuePattern`) are read and written with `WM_GETTEXT` and
//! `EM_REPLACESEL`, which reach a background window without focusing it.
//! A document with only a `TextPattern` can be read but typed into only
//! through keys, so `insert_text` refuses unless the app is in front.
//! Password fields (`IsPassword`) never give their value.
//!
//! The behaviour follows Orca's Windows runtime
//! (github.com/stablyai/orca, `native/computer-use-windows/runtime.ps1`),
//! which taught the pitfalls handled here: WPF resets its caret when a
//! value is written (`set_caret`), multiline edits have no `ValuePattern`,
//! and WinForms controls answer messages better than patterns.
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
//! Reading a node takes about twenty cross-process calls, so a full
//! 1200-element snapshot takes a few seconds. A UIA cache request would
//! cut that to a handful; it is future work.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationExpandCollapsePattern,
    IUIAutomationInvokePattern, IUIAutomationRangeValuePattern, IUIAutomationScrollItemPattern,
    IUIAutomationSelectionItemPattern, IUIAutomationTextPattern, IUIAutomationTogglePattern,
    IUIAutomationTreeWalker, IUIAutomationValuePattern, TextPatternRangeEndpoint_End,
    TextPatternRangeEndpoint_Start, TextUnit_Character, UIA_ExpandCollapsePatternId,
    UIA_InvokePatternId, UIA_RangeValuePatternId, UIA_ScrollItemPatternId,
    UIA_SelectionItemPatternId, UIA_TextPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, GetWindowLongW, IsIconic, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_GETTEXT,
    WM_GETTEXTLENGTH,
};
use windows::core::{BSTR, Interface};

use super::wm;
use crate::accessibility::{AxApp, AxBackend, Kind, NodeInfo, WindowMeta};
use crate::error::CallError;
use crate::hotkey::Chord;
use crate::win::{self, ExpandState, Patterns, UiaAction, UiaFault};

const EM_SETSEL: u32 = 0x00b1;
const EM_REPLACESEL: u32 = 0x00c2;
const ES_READONLY: u32 = 0x0800;
/// How long a message to another process may take.
const MESSAGE_TIMEOUT_MS: u32 = 1000;
/// Characters read from an edit control.
const EDIT_READ_LIMIT: usize = 1 << 18;
/// Characters of a document read through its `TextPattern`.
const TEXT_PATTERN_LIMIT: i32 = 1 << 16;
/// Children one element gives at most, so a broken provider can't hold a
/// read forever.
const MAX_CHILDREN: usize = 2000;
/// Levels and elements the menu bar search looks at.
const MENU_BAR_DEPTH: usize = 5;
const MENU_BAR_NODES: usize = 400;
/// A menu needs a moment to open after `Expand`.
const MENU_OPEN: Duration = Duration::from_millis(150);

/// `E_POINTER`: what the generated bindings answer for a null interface,
/// such as the sibling after the last one.
const E_POINTER: i32 = 0x8000_4003_u32 as i32;

/// One UI Automation element.
#[derive(Clone)]
pub struct Node(IUIAutomationElement);

// SAFETY: `Uia` creates the client objects in the multithreaded
// apartment, and every thread that touches them joined it (`ensure_com`).
// MTA interface pointers are free-threaded: calls from any MTA thread are
// legal and marshalled by UIA to the provider.
unsafe impl Send for Node {}
// SAFETY: see above.
unsafe impl Sync for Node {}

struct Automation {
    uia: IUIAutomation,
    walker: IUIAutomationTreeWalker,
}

// SAFETY: as for `Node`, both are MTA objects.
unsafe impl Send for Automation {}
// SAFETY: as for `Node`, both are MTA objects.
unsafe impl Sync for Automation {}

/// The UI Automation backend. The automation object is created on first
/// use.
pub struct Uia {
    automation: Mutex<Option<Arc<Automation>>>,
}

impl Uia {
    pub fn new() -> Self {
        wm::ensure_dpi_aware();
        Self {
            automation: Mutex::new(None),
        }
    }

    fn automation(&self) -> Result<Arc<Automation>, CallError> {
        ensure_com();
        let mut slot = self.automation.lock().unwrap();
        if let Some(a) = slot.as_ref() {
            return Ok(a.clone());
        }
        // SAFETY: COM is initialised on this thread; the class is the
        // in-process UI Automation client.
        let uia: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.map_err(
                |e| {
                    CallError::Failed(format!(
                        "accessibility_error: UI Automation is not available: {e}"
                    ))
                },
            )?;
        // SAFETY: a plain getter on the object just made.
        let walker = unsafe { uia.ControlViewWalker() }.map_err(err)?;
        let a = Arc::new(Automation { uia, walker });
        *slot = Some(a.clone());
        Ok(a)
    }
}

impl Default for Uia {
    fn default() -> Self {
        Self::new()
    }
}

/// Joins this thread to the multithreaded apartment, once. The blocking
/// pool's threads outlive calls, and the join ends with the thread. A
/// thread that is in another apartment already keeps it; UIA still works
/// there, through marshalling.
fn ensure_com() {
    thread_local! {
        static JOINED: Cell<bool> = const { Cell::new(false) };
    }
    JOINED.with(|joined| {
        if !joined.get() {
            // SAFETY: no pointers; a second call with another model fails
            // harmlessly.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            joined.set(true);
        }
    });
}

fn failure(e: &windows::core::Error) -> UiaFault {
    win::classify_hresult(e.code().0)
}

fn err(e: windows::core::Error) -> CallError {
    match failure(&e) {
        UiaFault::Gone => {
            CallError::Failed("element_not_found: the element is gone; take a new snapshot".into())
        }
        UiaFault::NotEnabled => CallError::Failed(
            "element_not_enabled: the element is disabled; do what enables it first".into(),
        ),
        UiaFault::NotClickable => {
            CallError::Failed("element_not_clickable: the element has no point to click".into())
        }
        UiaFault::NotSupported => {
            CallError::Failed("action_not_supported: the element does not support it".into())
        }
        UiaFault::Denied => CallError::Failed(
            "accessibility_error: access denied; the app may run as administrator, which the \
             node can read only when it runs as administrator too"
                .into(),
        ),
        UiaFault::Other => CallError::Failed(format!("accessibility_error: {e}")),
    }
}

fn text(b: windows::core::Result<BSTR>) -> Option<String> {
    let s = b.ok()?.to_string();
    (!s.is_empty()).then_some(s)
}

/// The pattern `id` of an element, when it supports it.
fn pattern<T: Interface>(
    e: &IUIAutomationElement,
    id: windows::Win32::UI::Accessibility::UIA_PATTERN_ID,
) -> Option<T> {
    // SAFETY: a getter; an unsupported pattern answers an error.
    unsafe { e.GetCurrentPatternAs::<T>(id) }.ok()
}

/// The runtime id as numbers.
fn runtime_id(e: &IUIAutomationElement) -> Option<Vec<i32>> {
    // SAFETY: the array is read while locked, then unlocked and freed, as
    // the SAFEARRAY contract asks; UIA's runtime ids are one-dimensional
    // arrays of 32-bit integers.
    unsafe {
        let psa = e.GetRuntimeId().ok()?;
        if psa.is_null() {
            return None;
        }
        let ids = (|| {
            let lo = SafeArrayGetLBound(psa, 1).ok()?;
            let hi = SafeArrayGetUBound(psa, 1).ok()?;
            let len = usize::try_from(hi - lo + 1).ok()?;
            let mut data: *mut c_void = std::ptr::null_mut();
            SafeArrayAccessData(psa, &raw mut data).ok()?;
            let ids = if data.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(data.cast::<i32>(), len).to_vec()
            };
            let _ = SafeArrayUnaccessData(psa);
            Some(ids)
        })();
        let _ = SafeArrayDestroy(psa);
        ids
    }
}

/// The native window of an edit or RichEdit control.
fn native_edit(e: &IUIAutomationElement) -> Option<HWND> {
    // SAFETY: plain getters.
    unsafe {
        let class = text(e.CurrentClassName())?;
        if !win::is_edit_class(&class) {
            return None;
        }
        let hwnd = e.CurrentNativeWindowHandle().ok()?;
        (!hwnd.is_invalid() && !hwnd.0.is_null()).then_some(hwnd)
    }
}

/// Sends a message another process answers; `None` when it did not in time.
fn send(hwnd: HWND, msg: u32, wparam: usize, lparam: isize) -> Option<usize> {
    let mut result = 0usize;
    // SAFETY: a message the control answers with the buffers the caller
    // passed in `lparam`, alive for the call; the timeout bounds it.
    let ok = unsafe {
        SendMessageTimeoutW(
            hwnd,
            msg,
            WPARAM(wparam),
            LPARAM(lparam),
            SMTO_ABORTIFHUNG,
            MESSAGE_TIMEOUT_MS,
            Some(&raw mut result),
        )
    };
    (ok.0 != 0).then_some(result)
}

fn edit_text(hwnd: HWND) -> Option<String> {
    let len = send(hwnd, WM_GETTEXTLENGTH, 0, 0)?.min(EDIT_READ_LIMIT);
    let mut buf = vec![0u16; len + 1];
    let copied = send(hwnd, WM_GETTEXT, buf.len(), buf.as_mut_ptr() as isize)?;
    Some(String::from_utf16_lossy(&buf[..copied.min(len)]))
}

fn edit_read_only(hwnd: HWND) -> bool {
    // SAFETY: a plain read of a window's style.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    style & ES_READONLY != 0
}

/// Replaces the control's selection with `text` through its own
/// `EM_REPLACESEL`, which runs its change notifications.
fn edit_replace_selection(hwnd: HWND, text: &str) -> Result<(), CallError> {
    if edit_read_only(hwnd) {
        return Err(CallError::Failed(
            "value_not_settable: the field is read-only".into(),
        ));
    }
    let rich = wm::class_name(hwnd).to_lowercase().contains("richedit");
    let mut wide: Vec<u16> = win::edit_newlines(text, !rich).encode_utf16().collect();
    wide.push(0);
    send(hwnd, EM_REPLACESEL, 1, wide.as_ptr() as isize).ok_or_else(|| {
        CallError::Failed(
            "window_not_responding: the text field did not answer in time; the text may still \
             land, so take a new snapshot before trying again"
                .into(),
        )
    })?;
    Ok(())
}

/// The selection in the element's text, in UTF-16 units, through its
/// `TextPattern`.
fn selection_offsets(e: &IUIAutomationElement) -> Option<(usize, usize)> {
    let tp: IUIAutomationTextPattern = pattern(e, UIA_TextPatternId)?;
    // SAFETY: getters and range arithmetic on objects this function owns.
    unsafe {
        let ranges = tp.GetSelection().ok()?;
        if ranges.Length().ok()? != 1 {
            return None;
        }
        let selected = ranges.GetElement(0).ok()?;
        let leading = tp.DocumentRange().ok()?.Clone().ok()?;
        leading
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &selected,
                TextPatternRangeEndpoint_Start,
            )
            .ok()?;
        let start = leading.GetText(-1).ok()?.to_string().encode_utf16().count();
        let len = selected
            .GetText(-1)
            .ok()?
            .to_string()
            .encode_utf16()
            .count();
        Some((start, start + len))
    }
}

/// Puts the caret after `leading`, the text before it. WPF resets the
/// caret to the start when a value is written, so the next insertion would
/// land there. Moves only when the range reads back as `leading`, because
/// providers count positions differently.
fn set_caret(e: &IUIAutomationElement, leading: &str) {
    let Some(tp) = pattern::<IUIAutomationTextPattern>(e, UIA_TextPatternId) else {
        return;
    };
    // UIA moves by caret positions, which keep a CRLF together.
    let positions = leading.chars().count() - leading.matches("\r\n").count();
    let Ok(positions) = i32::try_from(positions) else {
        return;
    };
    // SAFETY: range arithmetic on objects this function owns.
    unsafe {
        let Ok(range) = tp.DocumentRange().and_then(|r| r.Clone()) else {
            return;
        };
        let moved = range
            .MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &range,
                TextPatternRangeEndpoint_Start,
            )
            .and_then(|()| {
                range.MoveEndpointByUnit(
                    TextPatternRangeEndpoint_End,
                    TextUnit_Character,
                    positions,
                )
            });
        if moved.is_err() {
            return;
        }
        if range.GetText(-1).is_ok_and(|t| t == leading)
            && range
                .MoveEndpointByRange(
                    TextPatternRangeEndpoint_Start,
                    &range,
                    TextPatternRangeEndpoint_End,
                )
                .is_ok()
        {
            let _ = range.Select();
        }
    }
}

fn patterns_of(
    e: &IUIAutomationElement,
    control_type: i32,
) -> (Patterns, Option<bool>, Option<bool>) {
    if win::uia_skips_patterns(control_type) {
        return (Patterns::default(), None, None);
    }
    let mut p = Patterns::default();
    // SAFETY: getters on a pattern the element just gave.
    unsafe {
        p.invoke = pattern::<IUIAutomationInvokePattern>(e, UIA_InvokePatternId).is_some();
        let toggled = pattern::<IUIAutomationTogglePattern>(e, UIA_TogglePatternId)
            .and_then(|t| t.CurrentToggleState().ok());
        p.toggle = toggled.is_some();
        let selected = pattern::<IUIAutomationSelectionItemPattern>(e, UIA_SelectionItemPatternId)
            .and_then(|s| s.CurrentIsSelected().ok().map(|b| b.as_bool()));
        p.select =
            pattern::<IUIAutomationSelectionItemPattern>(e, UIA_SelectionItemPatternId).is_some();
        let expand = pattern::<IUIAutomationExpandCollapsePattern>(e, UIA_ExpandCollapsePatternId)
            .and_then(|x| x.CurrentExpandCollapseState().ok());
        p.expand = expand.and_then(|s| win::expand_state(s.0));
        p.scroll_item =
            pattern::<IUIAutomationScrollItemPattern>(e, UIA_ScrollItemPatternId).is_some();
        // Toggle state 1 is On.
        let on = toggled.map(|t| t.0 == 1);
        (p, on.or(selected), toggled.map(|t| t.0 == 2))
    }
}

impl Automation {
    fn first_child(&self, e: &IUIAutomationElement) -> Option<IUIAutomationElement> {
        // SAFETY: a tree walk; a null child is an `E_POINTER` error.
        unsafe { self.walker.GetFirstChildElement(e) }.ok()
    }

    fn next_sibling(&self, e: &IUIAutomationElement) -> Option<IUIAutomationElement> {
        // SAFETY: as above.
        unsafe { self.walker.GetNextSiblingElement(e) }.ok()
    }
}

/// The element's own value, as a line shows it: the `ValuePattern`, else
/// the range value, else (for text controls) the edit's text or the
/// document's. Never for a password.
fn read_value(e: &IUIAutomationElement, kind: Kind) -> Option<String> {
    // SAFETY: getters.
    unsafe {
        if let Some(v) = pattern::<IUIAutomationValuePattern>(e, UIA_ValuePatternId)
            && let Ok(s) = v.CurrentValue()
        {
            return Some(s.to_string());
        }
        if let Some(r) = pattern::<IUIAutomationRangeValuePattern>(e, UIA_RangeValuePatternId)
            && let Ok(v) = r.CurrentValue()
        {
            return Some(win::fmt_number(v));
        }
        if kind.editable() {
            if let Some(hwnd) = native_edit(e)
                && let Some(s) = edit_text(hwnd)
            {
                return Some(s);
            }
            if let Some(tp) = pattern::<IUIAutomationTextPattern>(e, UIA_TextPatternId)
                && let Ok(range) = tp.DocumentRange()
                && let Ok(s) = range.GetText(TEXT_PATTERN_LIMIT)
            {
                return Some(s.to_string());
            }
        }
    }
    None
}

impl AxBackend for Uia {
    type Node = Node;

    fn apps(&self) -> Result<Vec<AxApp>, CallError> {
        let front = wm::foreground().map(wm::pid);
        let mut apps: Vec<AxApp> = Vec::new();
        for hwnd in wm::app_windows()? {
            let pid = wm::pid(hwnd);
            if apps.iter().any(|a| a.pid == pid as i32) {
                continue;
            }
            apps.push(AxApp {
                name: wm::app_name(pid),
                bundle_id: None,
                pid: pid as i32,
                frontmost: front == Some(pid),
            });
        }
        Ok(apps)
    }

    fn windows(&self, pid: i32) -> Result<Vec<(Node, WindowMeta)>, CallError> {
        let auto = self.automation()?;
        let front = wm::foreground();
        let mut out = Vec::new();
        for hwnd in wm::app_windows()? {
            if wm::pid(hwnd) as i32 != pid {
                continue;
            }
            // SAFETY: a lookup by window handle; the window may have
            // closed since the list.
            let Ok(element) = (unsafe { auto.uia.ElementFromHandle(hwnd) }) else {
                continue;
            };
            out.push((
                Node(element),
                WindowMeta {
                    title: wm::title(hwnd),
                    frame: wm::frame(hwnd).ok(),
                    focused: front == Some(hwnd),
                    // SAFETY: a plain read.
                    minimized: unsafe { IsIconic(hwnd) }.as_bool(),
                },
            ));
        }
        Ok(out)
    }

    fn info(&self, node: &Node) -> Result<NodeInfo, CallError> {
        let e = &node.0;
        // SAFETY: getters on a live element; the first one tells whether
        // it is still there.
        unsafe {
            let control_type = e.CurrentControlType().map_err(err)?.0;
            let kind = win::uia_kind(control_type);
            let role = win::uia_role(
                &text(e.CurrentLocalizedControlType()).unwrap_or_default(),
                control_type,
            );
            let name = text(e.CurrentName());
            let identity = match runtime_id(e) {
                Some(ids) if !ids.is_empty() => win::runtime_id_hash(&ids),
                // A provider without runtime ids: name and type are the
                // best we have.
                _ => win::runtime_id_hash(
                    &name
                        .as_deref()
                        .unwrap_or("")
                        .encode_utf16()
                        .map(i32::from)
                        .chain([control_type])
                        .collect::<Vec<_>>(),
                ),
            };
            let mut info = NodeInfo::new(kind, role, identity);
            info.name = name;
            info.description = text(e.CurrentHelpText());
            info.enabled = e.CurrentIsEnabled().is_ok_and(|b| b.as_bool());
            info.focused = e.CurrentHasKeyboardFocus().is_ok_and(|b| b.as_bool());
            info.frame = e
                .CurrentBoundingRectangle()
                .ok()
                .and_then(|r| win::ax_frame(wm::edges(r)));
            let password = e.CurrentIsPassword().is_ok_and(|b| b.as_bool());
            let (patterns, selected, mixed) = patterns_of(e, control_type);
            info.actions = win::uia_actions(&patterns);
            info.selected = selected.unwrap_or(false);
            info.expanded = match patterns.expand {
                Some(ExpandState::Collapsed) => Some(false),
                Some(ExpandState::Expanded | ExpandState::Partial) => Some(true),
                None => None,
            };
            // The value of a password field is never read.
            info.value = if password {
                None
            } else if mixed == Some(true) {
                Some("mixed".into())
            } else {
                read_value(e, kind)
            };
            Ok(info)
        }
    }

    fn children(&self, node: &Node) -> Result<Vec<Node>, CallError> {
        let auto = self.automation()?;
        let mut out = Vec::new();
        // The first call tells a vanished element from one with no children.
        // SAFETY: a tree walk.
        let first = match unsafe { auto.walker.GetFirstChildElement(&node.0) } {
            Ok(c) => Some(c),
            Err(e) if e.code().0 == E_POINTER => None,
            Err(e) if failure(&e) == UiaFault::Gone => return Err(err(e)),
            Err(_) => None,
        };
        let mut next = first;
        while let Some(c) = next {
            if out.len() >= MAX_CHILDREN {
                break;
            }
            next = auto.next_sibling(&c);
            out.push(Node(c));
        }
        Ok(out)
    }

    fn focused(&self, pid: i32) -> Option<Node> {
        // Keyboard focus is in the window in front, so an app behind has
        // no focused element to report.
        let front = wm::foreground()?;
        if wm::pid(front) as i32 != pid {
            return None;
        }
        let auto = self.automation().ok()?;
        // SAFETY: a getter.
        unsafe { auto.uia.GetFocusedElement() }.ok().map(Node)
    }

    fn menu_bar(&self, pid: i32) -> Option<Node> {
        let auto = self.automation().ok()?;
        let windows = self.windows(pid).ok()?;
        let (window, _) = windows
            .iter()
            .find(|(_, w)| w.focused)
            .or_else(|| windows.first())?;
        // Breadth first: the bar is a child of the window or near it.
        let mut level = vec![window.0.clone()];
        let mut seen = 0;
        for _ in 0..MENU_BAR_DEPTH {
            let mut next = Vec::new();
            for e in &level {
                let mut child = auto.first_child(e);
                while let Some(c) = child {
                    seen += 1;
                    // SAFETY: a getter.
                    if unsafe { c.CurrentControlType() }.is_ok_and(|t| t.0 == 50010) {
                        return Some(Node(c));
                    }
                    if seen >= MENU_BAR_NODES {
                        return None;
                    }
                    child = auto.next_sibling(&c);
                    next.push(c);
                }
            }
            if next.is_empty() {
                return None;
            }
            level = next;
        }
        None
    }

    fn press(&self, node: &Node) -> Result<(), CallError> {
        let e = &node.0;
        // SAFETY: getters.
        let control_type = unsafe { e.CurrentControlType() }.map_err(err)?.0;
        let (patterns, _, _) = patterns_of(e, control_type);
        match win::press_action(&patterns) {
            Some(action) => run(e, action),
            None => Err(CallError::Failed(
                "element_not_clickable: the element offers no invoke, toggle, select or expand \
                 action; look for a button or item inside it"
                    .into(),
            )),
        }
    }

    fn perform(&self, node: &Node, action: &str) -> Result<(), CallError> {
        let Some(action) = win::parse_uia_action(action) else {
            return Err(CallError::Failed(format!("action_not_supported: {action}")));
        };
        run(&node.0, action)
    }

    fn set_value(&self, node: &Node, value: &str) -> Result<(), CallError> {
        let e = &node.0;
        // SAFETY: getters and setters of patterns the element gave.
        unsafe {
            // Edit controls first: a pattern write focuses the field,
            // which raises a background window; selecting everything and
            // replacing it also runs the control's change notification.
            if let Some(hwnd) = native_edit(e) {
                if edit_read_only(hwnd) {
                    return Err(not_settable("the field is read-only"));
                }
                if send(hwnd, EM_SETSEL, 0, -1).is_some() {
                    return edit_replace_selection(hwnd, value);
                }
            }
            if let Some(v) = pattern::<IUIAutomationValuePattern>(e, UIA_ValuePatternId) {
                if v.CurrentIsReadOnly().is_ok_and(|b| b.as_bool()) {
                    return Err(not_settable("the field is read-only"));
                }
                v.SetValue(&BSTR::from(value)).map_err(err)?;
                set_caret(e, value);
                return Ok(());
            }
            if let Some(r) = pattern::<IUIAutomationRangeValuePattern>(e, UIA_RangeValuePatternId) {
                if r.CurrentIsReadOnly().is_ok_and(|b| b.as_bool()) {
                    return Err(not_settable("the control is read-only"));
                }
                let n: f64 = value.trim().parse().map_err(|_| {
                    CallError::Failed(format!(
                        "invalid_argument: {value:?} is not a number, and this control takes one"
                    ))
                })?;
                let (min, max) = (
                    r.CurrentMinimum().map_err(err)?,
                    r.CurrentMaximum().map_err(err)?,
                );
                if n.is_nan() || n < min || n > max {
                    return Err(CallError::Failed(format!(
                        "invalid_argument: {value} is outside the control's range {}..{}",
                        win::fmt_number(min),
                        win::fmt_number(max)
                    )));
                }
                return r.SetValue(n).map_err(err);
            }
        }
        Err(not_settable("it has no value to set"))
    }

    fn insert_text(&self, node: &Node, text: &str) -> Result<(), CallError> {
        let e = &node.0;
        // SAFETY: getters and setters of patterns the element gave.
        unsafe {
            if let Some(hwnd) = native_edit(e) {
                return edit_replace_selection(hwnd, text);
            }
            if let Some(v) = pattern::<IUIAutomationValuePattern>(e, UIA_ValuePatternId) {
                if v.CurrentIsReadOnly().is_ok_and(|b| b.as_bool()) {
                    return Err(not_settable("the field is read-only"));
                }
                if e.CurrentIsPassword().is_ok_and(|b| b.as_bool()) {
                    return Err(not_settable(
                        "it is a password field, whose text the node does not read; use set-value \
                         to replace it",
                    ));
                }
                let current = v.CurrentValue().map_err(err)?.to_string();
                let selection = selection_offsets(e);
                let next = win::splice_utf16(&current, selection, text);
                v.SetValue(&BSTR::from(next.as_str())).map_err(err)?;
                // Where the caret was, plus the new text; a caret we
                // can't read means the insertion went to the end.
                let end = selection.map_or_else(
                    || next.clone(),
                    |(start, _)| {
                        let units: Vec<u16> = next.encode_utf16().collect();
                        let upto = (start + text.encode_utf16().count()).min(units.len());
                        String::from_utf16_lossy(&units[..upto])
                    },
                );
                set_caret(e, &end);
                return Ok(());
            }
            // No pattern writes text here: only keys can, and only the
            // window in front gets them.
            let front = wm::foreground().map(wm::pid);
            let pid = e.CurrentProcessId().map_err(err)? as u32;
            if front != Some(pid) {
                return Err(CallError::Failed(
                    "background_unavailable: this field takes text only as keys, which go only to \
                     the window in front; bring the app forward first"
                        .into(),
                ));
            }
            let native = e.CurrentNativeWindowHandle().ok();
            if let Some(h) = native.filter(|h| !h.is_invalid() && !h.0.is_null()) {
                let _ = SetFocus(Some(h));
            }
            e.SetFocus().map_err(|_| {
                CallError::Failed("no_focus: the element did not take keyboard focus".into())
            })?;
            super::keyboard::type_text(text)
        }
    }

    fn press_key(&self, pid: i32, chord: &Chord) -> Result<(), CallError> {
        let front = wm::foreground().map(wm::pid);
        if front != u32::try_from(pid).ok() {
            return Err(CallError::Failed(
                "background_unavailable: keys go only to the window in front, and this app is \
                 behind; bring it forward first"
                    .into(),
            ));
        }
        super::keyboard::keystroke(chord)
    }

    fn expand(&self, node: &Node) -> Result<(), CallError> {
        if let Some(x) =
            pattern::<IUIAutomationExpandCollapsePattern>(&node.0, UIA_ExpandCollapsePatternId)
        {
            // SAFETY: a call on a pattern the element gave.
            unsafe { x.Expand() }.map_err(err)?;
            std::thread::sleep(MENU_OPEN);
        }
        Ok(())
    }
}

fn not_settable(why: &str) -> CallError {
    CallError::Failed(format!("value_not_settable: {why}"))
}

/// Runs one pattern action.
fn run(e: &IUIAutomationElement, action: UiaAction) -> Result<(), CallError> {
    let unsupported = || {
        CallError::Failed(format!(
            "action_not_supported: the element has no {} action",
            action.name()
        ))
    };
    // SAFETY: calls on patterns the element gave.
    unsafe {
        match action {
            UiaAction::Invoke => pattern::<IUIAutomationInvokePattern>(e, UIA_InvokePatternId)
                .ok_or_else(unsupported)?
                .Invoke(),
            UiaAction::Toggle => pattern::<IUIAutomationTogglePattern>(e, UIA_TogglePatternId)
                .ok_or_else(unsupported)?
                .Toggle(),
            UiaAction::Select => {
                pattern::<IUIAutomationSelectionItemPattern>(e, UIA_SelectionItemPatternId)
                    .ok_or_else(unsupported)?
                    .Select()
            }
            UiaAction::Expand => {
                pattern::<IUIAutomationExpandCollapsePattern>(e, UIA_ExpandCollapsePatternId)
                    .ok_or_else(unsupported)?
                    .Expand()
            }
            UiaAction::Collapse => {
                pattern::<IUIAutomationExpandCollapsePattern>(e, UIA_ExpandCollapsePatternId)
                    .ok_or_else(unsupported)?
                    .Collapse()
            }
            UiaAction::ScrollIntoView => {
                pattern::<IUIAutomationScrollItemPattern>(e, UIA_ScrollItemPatternId)
                    .ok_or_else(unsupported)?
                    .ScrollIntoView()
            }
        }
    }
    .map_err(err)
}
