//! `accessibility` on `AXUIElement`: the backend under
//! [`crate::accessibility::Accessibility`]. Elements are read attribute by
//! attribute, compared by `CFHash` and `CFEqual`, and acted on through
//! their AX actions, so nothing moves the pointer or raises a window. Keys
//! go to the app's process with `CGEventPostToPid`, also in the background.
//!
//! Secure text fields never show their value.

use std::ptr::NonNull;
use std::sync::Once;

use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
use objc2_application_services::{AXUIElement, AXValueType};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFEqual, CFHash, CFNumber, CFRetained, CFString, CFType, CFURL, CGPoint,
    CGSize,
};

use super::window::{AxErr, attr, ax_value, check, element_attr, elements_attr, string_attr};
use crate::accessibility::{AxApp, AxBackend, Kind, NodeInfo, WindowMeta, render};
use crate::error::CallError;
use crate::hotkey::Chord;
use crate::window::Rect;

/// One element, shared across threads: CF retains are atomic and AX calls
/// are thread-safe.
#[derive(Clone)]
pub struct Node(CFRetained<AXUIElement>);

// SAFETY: see above.
unsafe impl Send for Node {}
// SAFETY: see above.
unsafe impl Sync for Node {}

pub struct MacAx;

fn err(e: AxErr) -> CallError {
    match e {
        AxErr::NotFound => CallError::Failed(
            "element_not_found: the element is gone; take a new snapshot".into(),
        ),
        AxErr::Denied => CallError::Failed(
            "accessibility_error: the node lacks the Accessibility permission (bricolo permissions --request)"
                .into(),
        ),
        AxErr::Failed(m) => CallError::Failed(format!("accessibility_error: {m}")),
    }
}

/// An app that does not answer holds every call for 6 seconds by default.
fn set_timeout() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let system = unsafe { AXUIElement::new_system_wide() };
        let _ = unsafe { system.set_messaging_timeout(2.0) };
    });
}

fn denied_unless_trusted() -> Result<(), CallError> {
    if super::accessibility_granted() {
        Ok(())
    } else {
        Err(err(AxErr::Denied))
    }
}

/// A string, a number or a boolean value as text.
fn text_of(value: &CFType) -> Option<String> {
    if let Some(s) = value.downcast_ref::<CFString>() {
        return Some(s.to_string());
    }
    if let Some(b) = value.downcast_ref::<CFBoolean>() {
        return Some(if b.as_bool() { "1" } else { "0" }.into());
    }
    if let Some(n) = value.downcast_ref::<CFNumber>() {
        let f = n.as_f64()?;
        return Some(if f.fract() == 0.0 && f.abs() < 1e15 {
            format!("{}", f as i64)
        } else {
            format!("{f}")
        });
    }
    if let Some(u) = value.downcast_ref::<CFURL>() {
        return Some(u.string().to_string());
    }
    None
}

fn value_attr(element: &AXUIElement, name: &str) -> Result<Option<String>, AxErr> {
    Ok(attr(element, name)?.and_then(|v| text_of(&v)))
}

fn bool_attr(element: &AXUIElement, name: &str) -> Option<bool> {
    let v = attr(element, name).ok()??;
    v.downcast_ref::<CFBoolean>().map(|b| b.as_bool())
}

fn frame(element: &AXUIElement) -> Option<Rect> {
    let mut origin = CGPoint::new(0.0, 0.0);
    let mut size = CGSize::new(0.0, 0.0);
    ax_value(element, "AXPosition", AXValueType::CGPoint, &mut origin).ok()?;
    ax_value(element, "AXSize", AXValueType::CGSize, &mut size).ok()?;
    Some(Rect {
        x: origin.x.round() as i32,
        y: origin.y.round() as i32,
        width: size.width.max(0.0).round() as u32,
        height: size.height.max(0.0).round() as u32,
    })
}

fn raw_actions(element: &AXUIElement) -> Vec<String> {
    let mut names: *const CFArray = std::ptr::null();
    let e = unsafe { element.copy_action_names(NonNull::from(&mut names)) };
    if check(e).is_err() {
        return Vec::new();
    }
    let Some(ptr) = NonNull::new(names as *mut CFArray) else {
        return Vec::new();
    };
    let array: CFRetained<CFArray> = unsafe { CFRetained::from_raw(ptr) };
    let array: CFRetained<CFArray<CFString>> = unsafe { CFRetained::cast_unchecked(array) };
    array.iter().map(|s| s.to_string()).collect()
}

fn settable(element: &AXUIElement, name: &str) -> bool {
    let mut yes: u8 = 0;
    let e = unsafe {
        element.is_attribute_settable(&CFString::from_str(name), NonNull::from(&mut yes))
    };
    check(e).is_ok() && yes != 0
}

/// The engine's kind for an AX role and subrole.
pub fn kind(role: &str, subrole: Option<&str>) -> Kind {
    match (role, subrole) {
        ("AXWindow", Some("AXDialog" | "AXSystemDialog" | "AXFloatingWindow")) => Kind::Dialog,
        ("AXWindow", _) => Kind::Window,
        ("AXSheet", _) => Kind::Dialog,
        ("AXApplication", _) => Kind::Application,
        ("AXRadioButton", Some("AXTabButton")) => Kind::Tab,
        ("AXButton", _) => Kind::Button,
        ("AXCheckBox", _) => Kind::CheckBox,
        ("AXRadioButton", _) => Kind::RadioButton,
        ("AXComboBox", _) => Kind::ComboBox,
        ("AXPopUpButton" | "AXMenuButton", _) => Kind::PopUpButton,
        ("AXDisclosureTriangle", _) => Kind::Disclosure,
        ("AXTextField", _) => Kind::TextField,
        ("AXTextArea", _) => Kind::TextArea,
        ("AXStaticText", _) => Kind::StaticText,
        ("AXHeading", _) => Kind::Heading,
        ("AXLink", _) => Kind::Link,
        ("AXImage", _) => Kind::Image,
        ("AXSlider" | "AXIncrementor" | "AXValueIndicator", _) => Kind::Slider,
        ("AXMenu", _) => Kind::Menu,
        ("AXMenuBar", _) => Kind::MenuBar,
        ("AXMenuBarItem", _) => Kind::MenuBarItem,
        ("AXMenuItem", _) => Kind::MenuItem,
        ("AXList" | "AXOutline" | "AXBrowser", _) => Kind::List,
        ("AXRow" | "AXOutlineRow", _) => Kind::Row,
        ("AXCell", _) => Kind::Cell,
        ("AXTable" | "AXGrid", _) => Kind::Table,
        ("AXTabGroup", _) => Kind::TabGroup,
        ("AXScrollArea", _) => Kind::ScrollArea,
        ("AXScrollBar", _) => Kind::ScrollBar,
        ("AXToolbar", _) => Kind::Toolbar,
        ("AXWebArea", _) => Kind::WebArea,
        ("AXGroup" | "AXSplitGroup" | "AXLayoutArea" | "AXLayoutItem" | "AXUnknown", _) => {
            Kind::Group
        }
        _ => Kind::Other,
    }
}

fn identity(element: &AXUIElement) -> u64 {
    let cf: &CFType = element.as_ref();
    CFHash(Some(cf)) as u64
}

fn same(a: &AXUIElement, b: &AXUIElement) -> bool {
    CFEqual(Some(a.as_ref()), Some(b.as_ref()))
}

fn read(element: &AXUIElement) -> Result<NodeInfo, AxErr> {
    let role = string_attr(element, "AXRole")?.ok_or(AxErr::NotFound)?;
    let subrole = string_attr(element, "AXSubrole").ok().flatten();
    let kind = kind(&role, subrole.as_deref());
    let role_text = string_attr(element, "AXRoleDescription")
        .ok()
        .flatten()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| render::split_camel(role.trim_start_matches("AX")));
    let mut info = NodeInfo::new(kind, role_text.to_lowercase(), identity(element));
    info.name = string_attr(element, "AXTitle").ok().flatten();
    info.description = string_attr(element, "AXDescription").ok().flatten();
    let secure = subrole.as_deref() == Some("AXSecureTextField");
    if !secure && !matches!(kind, Kind::Window | Kind::Dialog | Kind::Application) {
        info.value = value_attr(element, "AXValue").ok().flatten();
    }
    if kind.editable() {
        info.placeholder = string_attr(element, "AXPlaceholderValue").ok().flatten();
    }
    if kind == Kind::Link {
        info.url = value_attr(element, "AXURL").ok().flatten();
    }
    info.enabled = bool_attr(element, "AXEnabled").unwrap_or(true);
    info.focused = bool_attr(element, "AXFocused").unwrap_or(false);
    if matches!(kind, Kind::Row | Kind::Cell | Kind::Tab | Kind::MenuItem) {
        info.selected = bool_attr(element, "AXSelected").unwrap_or(false);
    }
    if matches!(kind, Kind::Row | Kind::Disclosure) {
        info.expanded = bool_attr(element, "AXDisclosing");
    } else if matches!(kind, Kind::ComboBox | Kind::PopUpButton) {
        info.expanded = bool_attr(element, "AXExpanded");
    }
    info.frame = frame(element);
    info.actions = raw_actions(element)
        .iter()
        .map(|a| render::pretty_action(a))
        .collect();
    Ok(info)
}

fn press_raw(element: &AXUIElement, raw: &str) -> Result<(), CallError> {
    check(unsafe { element.perform_action(&CFString::from_str(raw)) }).map_err(err)
}

impl AxBackend for MacAx {
    type Node = Node;

    fn apps(&self) -> Result<Vec<AxApp>, CallError> {
        denied_unless_trusted()?;
        set_timeout();
        Ok(objc2::rc::autoreleasepool(|_| {
            NSWorkspace::sharedWorkspace()
                .runningApplications()
                .iter()
                .filter(|a| {
                    a.activationPolicy() == NSApplicationActivationPolicy::Regular
                        && a.processIdentifier() > 0
                })
                .map(|a| AxApp {
                    name: a.localizedName().map(|n| n.to_string()).unwrap_or_default(),
                    bundle_id: a.bundleIdentifier().map(|b| b.to_string()),
                    pid: a.processIdentifier(),
                    frontmost: a.isActive(),
                })
                .collect()
        }))
    }

    fn windows(&self, pid: i32) -> Result<Vec<(Node, WindowMeta)>, CallError> {
        denied_unless_trusted()?;
        let app = unsafe { AXUIElement::new_application(pid) };
        let focused = element_attr(&app, "AXFocusedWindow").ok().flatten();
        let windows = elements_attr(&app, "AXWindows").map_err(err)?;
        Ok(windows
            .into_iter()
            .map(|w| {
                let meta = WindowMeta {
                    title: string_attr(&w, "AXTitle")
                        .ok()
                        .flatten()
                        .unwrap_or_default(),
                    frame: frame(&w),
                    focused: focused.as_ref().is_some_and(|f| same(f, &w)),
                    minimized: bool_attr(&w, "AXMinimized").unwrap_or(false),
                };
                (Node(w), meta)
            })
            .collect())
    }

    fn info(&self, node: &Node) -> Result<NodeInfo, CallError> {
        read(&node.0).map_err(err)
    }

    fn children(&self, node: &Node) -> Result<Vec<Node>, CallError> {
        Ok(elements_attr(&node.0, "AXChildren")
            .map_err(err)?
            .into_iter()
            .map(Node)
            .collect())
    }

    fn focused(&self, pid: i32) -> Option<Node> {
        let app = unsafe { AXUIElement::new_application(pid) };
        element_attr(&app, "AXFocusedUIElement")
            .ok()
            .flatten()
            .map(Node)
    }

    fn menu_bar(&self, pid: i32) -> Option<Node> {
        let app = unsafe { AXUIElement::new_application(pid) };
        element_attr(&app, "AXMenuBar").ok().flatten().map(Node)
    }

    fn press(&self, node: &Node) -> Result<(), CallError> {
        let actions = raw_actions(&node.0);
        if actions.iter().any(|a| a == "AXPress") {
            return press_raw(&node.0, "AXPress");
        }
        // A list or table row with no press action is picked by selecting
        // it, as Orca does.
        if settable(&node.0, "AXSelected") {
            let yes = CFBoolean::new(true);
            return check(unsafe {
                node.0
                    .set_attribute_value(&CFString::from_str("AXSelected"), yes)
            })
            .map_err(err);
        }
        for fallback in ["AXConfirm", "AXPick", "AXOpen"] {
            if actions.iter().any(|a| a == fallback) {
                return press_raw(&node.0, fallback);
            }
        }
        Err(CallError::Failed(
            "element_not_clickable: the element has no press action; try its parent or a listed action"
                .into(),
        ))
    }

    fn perform(&self, node: &Node, action: &str) -> Result<(), CallError> {
        let raw = raw_actions(&node.0)
            .into_iter()
            .find(|a| render::pretty_action(a).eq_ignore_ascii_case(action))
            .ok_or_else(|| CallError::Failed(format!("action_not_supported: {action}")))?;
        press_raw(&node.0, &raw)
    }

    fn set_value(&self, node: &Node, value: &str) -> Result<(), CallError> {
        let e = &node.0;
        if !settable(e, "AXValue") {
            return Err(CallError::Failed(
                "value_not_settable: the element's value cannot be set; click it or type into it"
                    .into(),
            ));
        }
        let name = CFString::from_str("AXValue");
        // Keep the type the element holds: numbers for sliders and
        // checkboxes, text otherwise.
        let current = attr(e, "AXValue").ok().flatten();
        let is_number = current
            .as_ref()
            .is_some_and(|c| c.downcast_ref::<CFNumber>().is_some());
        let is_bool = current
            .as_ref()
            .is_some_and(|c| c.downcast_ref::<CFBoolean>().is_some());
        let result = if is_bool {
            let on = matches!(
                value.trim().to_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            );
            unsafe { e.set_attribute_value(&name, CFBoolean::new(on)) }
        } else if is_number {
            let n: f64 = value.trim().parse().map_err(|_| {
                CallError::Failed(format!("invalid_argument: {value:?} is not a number"))
            })?;
            let number = CFNumber::new_f64(n);
            unsafe { e.set_attribute_value(&name, &number) }
        } else {
            let text = CFString::from_str(value);
            unsafe { e.set_attribute_value(&name, &text) }
        };
        check(result).map_err(err)
    }

    fn insert_text(&self, node: &Node, text: &str) -> Result<(), CallError> {
        let e = &node.0;
        if settable(e, "AXSelectedText") {
            let t = CFString::from_str(text);
            return check(unsafe {
                e.set_attribute_value(&CFString::from_str("AXSelectedText"), &t)
            })
            .map_err(err);
        }
        let old = value_attr(e, "AXValue").ok().flatten().unwrap_or_default();
        self.set_value(node, &format!("{old}{text}"))
    }

    fn press_key(&self, pid: i32, chord: &Chord) -> Result<(), CallError> {
        super::keyboard::press_to_pid(pid, chord)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessibility::{Accessibility, AccessibilityProvider};
    use crate::caller::Caller;

    #[test]
    fn maps_roles_to_kinds() {
        assert_eq!(kind("AXWindow", Some("AXStandardWindow")), Kind::Window);
        assert_eq!(kind("AXWindow", Some("AXDialog")), Kind::Dialog);
        assert_eq!(kind("AXRadioButton", Some("AXTabButton")), Kind::Tab);
        assert_eq!(kind("AXTextField", Some("AXSearchField")), Kind::TextField);
        assert_eq!(kind("AXSplitGroup", None), Kind::Group);
        assert_eq!(kind("AXRuler", None), Kind::Other);
    }

    /// Opens a text file in TextEdit in the background, reads its tree,
    /// types into the document and reads the text back. Needs the
    /// Accessibility permission; run with `--run-ignored all` on a Mac.
    #[ignore = "needs the Accessibility permission and a GUI session"]
    #[tokio::test]
    async fn reads_and_types_into_textedit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bricolo-ax-test.txt");
        std::fs::write(&file, "hello").unwrap();
        let _ = std::process::Command::new("/usr/bin/open")
            .args(["-g", "-a", "TextEdit"])
            .arg(&file)
            .status();
        let ax = Accessibility::new(MacAx, None);
        let c = Caller::new("test");
        let app = "com.apple.TextEdit".to_string();
        let found = ax
            .wait(&c, app.clone(), "bricolo-ax-test".into(), Some(10_000))
            .await
            .unwrap();
        assert!(!found.is_empty());
        let windows = ax.list_windows(&c, app.clone()).await.unwrap();
        let w = windows
            .iter()
            .find(|w| w.title.contains("bricolo-ax-test"))
            .unwrap()
            .index;
        let s = ax
            .snapshot(&c, app.clone(), Some(w), None, None, None)
            .await
            .unwrap();
        let line = s
            .tree
            .lines()
            .find(|l| l.contains("text entry area"))
            .unwrap_or_else(|| panic!("{}", s.tree));
        let index: u32 = line.split_whitespace().next().unwrap().parse().unwrap();
        let r = ax
            .set_value(&c, app.clone(), index, "typed by bricolo".into())
            .await
            .unwrap();
        assert_eq!(
            r.verification,
            crate::accessibility::Verification::Verified,
            "{r:?}"
        );
        // Close the document without saving: cmd+w would be refused only
        // for quitting; use the menu instead.
        let _ = std::process::Command::new("/usr/bin/osascript")
            .args([
                "-e",
                "tell application \"TextEdit\" to close (every document whose name contains \"bricolo-ax-test\") saving no",
            ])
            .status();
    }
}
