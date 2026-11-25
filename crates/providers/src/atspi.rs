//! The pure parts of the AT-SPI backend (`linux/accessibility.rs`): role
//! names to [`Kind`], state bits, which actions to press, what counts as a
//! window or a secure field, and how a frame is read. They hold no D-Bus
//! or Linux code, so the tests run on every system.
//!
//! The choices follow Orca's computer use (github.com/stablyai/orca,
//! `native/computer-use-linux/runtime.py`, MIT, Copyright (c) 2026
//! Lovecast Inc.): the actions it presses first, the windows it lists, the
//! words that mark a secret field, and the 500-character text limit.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::accessibility::{Kind, render};
use crate::window::Rect;

/// The path AT-SPI uses for "no object".
pub const NULL_PATH: &str = "/org/a11y/atspi/null";
/// Characters of a text value one read takes.
pub const TEXT_LIMIT: i32 = 500;

/// `AtspiStateType` numbers, from atspi-constants.h.
#[allow(dead_code)]
pub mod state {
    pub const ACTIVE: u32 = 1;
    pub const CHECKED: u32 = 4;
    pub const COLLAPSED: u32 = 5;
    pub const DEFUNCT: u32 = 6;
    pub const EDITABLE: u32 = 7;
    pub const ENABLED: u32 = 8;
    pub const EXPANDABLE: u32 = 9;
    pub const EXPANDED: u32 = 10;
    pub const FOCUSED: u32 = 12;
    pub const ICONIFIED: u32 = 15;
    pub const MULTI_LINE: u32 = 17;
    pub const SELECTABLE: u32 = 22;
    pub const SELECTED: u32 = 23;
    pub const SENSITIVE: u32 = 24;
    pub const SHOWING: u32 = 25;
    pub const CHECKABLE: u32 = 41;
}

/// The 64 state bits `GetState` returns as two `u32` words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct States(pub u64);

impl States {
    pub fn from_words(words: &[u32]) -> Self {
        let low = u64::from(words.first().copied().unwrap_or(0));
        let high = u64::from(words.get(1).copied().unwrap_or(0));
        Self(low | high << 32)
    }

    pub fn has(self, state: u32) -> bool {
        state < 64 && self.0 & (1 << state) != 0
    }

    /// Toolkits set ENABLED and SENSITIVE on usable elements. A set with
    /// neither bit and nothing else (the states could not be read) is not
    /// called disabled.
    pub fn usable(self) -> bool {
        self.0 == 0 || self.has(state::ENABLED) || self.has(state::SENSITIVE)
    }
}

/// What the parent says an element is. AT-SPI gives a menu bar's items
/// and a submenu's entry the same role as a menu; the engine tells them
/// apart by [`Kind`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Hint {
    #[default]
    None,
    /// A child of a menu bar.
    BarItem,
    /// A child of a menu.
    MenuItem,
}

/// The hint for the children of an element with role `parent`.
pub fn hint_for_children(parent: &str) -> Hint {
    match parent {
        "menu bar" => Hint::BarItem,
        "menu" | "popup menu" => Hint::MenuItem,
        _ => Hint::None,
    }
}

/// The engine's kind for an AT-SPI role name (`GetRoleName`).
pub fn kind(role: &str, states: States, hint: Hint) -> Kind {
    match role {
        "application" => Kind::Application,
        "frame" | "window" | "internal frame" => Kind::Window,
        "dialog" | "alert" | "file chooser" | "color chooser" | "font chooser" | "option pane" => {
            Kind::Dialog
        }
        "push button" | "toggle button" | "push button menu" => Kind::Button,
        "check box" => Kind::CheckBox,
        "radio button" => Kind::RadioButton,
        "combo box" => Kind::ComboBox,
        "page tab" => Kind::Tab,
        "page tab list" => Kind::TabGroup,
        "entry" | "password text" | "spin button" | "date editor" | "dateeditor" => Kind::TextField,
        "text" => {
            if !states.has(state::EDITABLE) {
                Kind::StaticText
            } else if states.has(state::MULTI_LINE) {
                Kind::TextArea
            } else {
                Kind::TextField
            }
        }
        "terminal" => Kind::TextArea,
        "document frame"
        | "document text"
        | "document email"
        | "document spreadsheet"
        | "document presentation" => {
            if states.has(state::EDITABLE) {
                Kind::TextArea
            } else {
                Kind::Group
            }
        }
        "document web" => Kind::WebArea,
        "label" | "static" | "caption" | "paragraph" | "footer" | "header" => Kind::StaticText,
        "heading" => Kind::Heading,
        "link" => Kind::Link,
        "image" | "icon" | "animation" | "image map" => Kind::Image,
        "slider" | "dial" | "level bar" => Kind::Slider,
        "scroll bar" => Kind::ScrollBar,
        "scroll pane" => Kind::ScrollArea,
        "menu" => match hint {
            Hint::BarItem => Kind::MenuBarItem,
            Hint::MenuItem => Kind::MenuItem,
            Hint::None => Kind::Menu,
        },
        "popup menu" => match hint {
            Hint::MenuItem => Kind::MenuItem,
            _ => Kind::Menu,
        },
        "menu bar" => Kind::MenuBar,
        "menu item" | "check menu item" | "radio menu item" | "tearoff menu item" => Kind::MenuItem,
        "list" | "list box" | "tree" => Kind::List,
        "list item" | "table row" | "tree item" => Kind::Row,
        "table cell"
        | "column header"
        | "row header"
        | "table column header"
        | "table row header" => Kind::Cell,
        "table" | "tree table" => Kind::Table,
        "tool bar" => Kind::Toolbar,
        "panel" | "filler" | "section" | "grouping" | "form" | "split pane" | "layered pane"
        | "root pane" | "glass pane" | "viewport" | "page" | "article" | "landmark"
        | "redundant object" | "status bar" | "separator" | "html container" | "info bar"
        | "notification" | "desktop frame" | "extended" | "unknown" | "invalid" => Kind::Group,
        _ => Kind::Other,
    }
}

/// Roles of a top-level window in an app's children.
pub fn is_window_role(role: &str) -> bool {
    matches!(
        role,
        "frame" | "window" | "dialog" | "alert" | "file chooser" | "color chooser" | "font chooser"
    )
}

/// Whether the element holds a yes/no state its value shows as 1 or 0.
pub fn is_checkable_role(role: &str) -> bool {
    matches!(
        role,
        "check box" | "radio button" | "toggle button" | "check menu item" | "radio menu item"
    )
}

/// Words that mark a field as holding a secret, as in Orca.
const SECRET_WORDS: &[&str] = &[
    "password",
    "passcode",
    "secret",
    "one-time code",
    "verification code",
];

/// Secure fields never show their value: password roles, and editable
/// fields whose name says they hold a secret.
pub fn is_secure(role: &str, name: Option<&str>, editable_kind: bool) -> bool {
    if role == "password text" {
        return true;
    }
    if !editable_kind {
        return false;
    }
    let label = format!("{role} {}", name.unwrap_or("")).to_lowercase();
    SECRET_WORDS.iter().any(|w| label.contains(w)) || has_word(&label, "pin")
}

fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|part| part == word)
}

/// The action as a line shows it: lowercase words.
pub fn display_action(name: &str, description: &str) -> Option<String> {
    let raw = if name.trim().is_empty() {
        description
    } else {
        name
    };
    let shown = render::pretty_action(raw.trim()).replace(['_', '-'], " ");
    let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    (!shown.is_empty()).then_some(shown)
}

/// The actions of an element as lines show them, without repeats.
pub fn action_names(actions: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (name, description) in actions {
        if let Some(shown) = display_action(name, description)
            && !out.contains(&shown)
        {
            out.push(shown);
        }
    }
    out
}

/// The index of the action that presses the element, as in Orca: the
/// first of the usual names, else one that mentions click, press or
/// activate.
pub fn preferred_action(actions: &[(String, String)]) -> Option<usize> {
    const FIRST: &[&str] = &[
        "click", "press", "activate", "invoke", "select", "toggle", "open",
    ];
    let names: Vec<String> = actions
        .iter()
        .map(|(n, d)| display_action(n, d).unwrap_or_default())
        .collect();
    names
        .iter()
        .position(|n| FIRST.contains(&n.as_str()))
        .or_else(|| {
            names
                .iter()
                .position(|n| ["click", "press", "activate"].iter().any(|t| n.contains(t)))
        })
}

/// The index of the action `wanted` names (as a line shows it, any case).
pub fn action_index(actions: &[(String, String)], wanted: &str) -> Option<usize> {
    let wanted = wanted.trim();
    actions.iter().position(|(n, d)| {
        n.eq_ignore_ascii_case(wanted)
            || display_action(n, d).is_some_and(|s| s.eq_ignore_ascii_case(wanted))
    })
}

/// Equal for the same (bus name, path) in every read.
pub fn identity(dest: &str, path: &str) -> u64 {
    let mut h = DefaultHasher::new();
    dest.hash(&mut h);
    path.hash(&mut h);
    h.finish()
}

/// A frame from `GetExtents` (x, y, width, height); none when the element
/// has no size.
pub fn frame(extents: (i32, i32, i32, i32)) -> Option<Rect> {
    let (x, y, width, height) = extents;
    (width > 0 && height > 0).then_some(Rect {
        x,
        y,
        width: width as u32,
        height: height as u32,
    })
}

/// Text read up to [`TEXT_LIMIT`] characters, with `...` after the cut.
pub fn clipped(text: String, count: i32) -> String {
    if count > TEXT_LIMIT {
        format!("{text}...")
    } else {
        text
    }
}

/// The text of a yes/no value: on, true, yes and 1.
pub fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "1" | "true" | "on" | "yes" | "checked"
    )
}

/// How a number reads back from a Value interface: `50` for `50.0`.
pub fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(bits: &[u32]) -> States {
        States(bits.iter().fold(0u64, |acc, b| acc | 1 << b))
    }

    #[test]
    fn state_words_join_into_one_set() {
        let s = States::from_words(&[1 << state::ENABLED, 1 << (41 - 32)]);
        assert!(s.has(state::ENABLED));
        assert!(s.has(state::CHECKABLE));
        assert!(!s.has(state::FOCUSED));
        assert!(States::from_words(&[]).usable());
    }

    #[test]
    fn disabled_means_neither_enabled_nor_sensitive() {
        assert!(states(&[state::ENABLED]).usable());
        assert!(states(&[state::SENSITIVE, state::SHOWING]).usable());
        assert!(!states(&[state::SHOWING]).usable());
    }

    #[test]
    fn roles_map_to_kinds() {
        let none = States::default();
        assert_eq!(kind("frame", none, Hint::None), Kind::Window);
        assert_eq!(kind("dialog", none, Hint::None), Kind::Dialog);
        assert_eq!(kind("push button", none, Hint::None), Kind::Button);
        assert_eq!(kind("check box", none, Hint::None), Kind::CheckBox);
        assert_eq!(kind("page tab", none, Hint::None), Kind::Tab);
        assert_eq!(kind("entry", none, Hint::None), Kind::TextField);
        assert_eq!(kind("password text", none, Hint::None), Kind::TextField);
        assert_eq!(kind("label", none, Hint::None), Kind::StaticText);
        assert_eq!(kind("filler", none, Hint::None), Kind::Group);
        assert_eq!(kind("level bar", none, Hint::None), Kind::Slider);
        assert_eq!(kind("something new", none, Hint::None), Kind::Other);
    }

    #[test]
    fn text_follows_its_states() {
        let one_line = states(&[state::EDITABLE]);
        let many = states(&[state::EDITABLE, state::MULTI_LINE]);
        assert_eq!(
            kind("text", States::default(), Hint::None),
            Kind::StaticText
        );
        assert_eq!(kind("text", one_line, Hint::None), Kind::TextField);
        assert_eq!(kind("text", many, Hint::None), Kind::TextArea);
        assert_eq!(kind("document frame", many, Hint::None), Kind::TextArea);
        assert_eq!(kind("document web", many, Hint::None), Kind::WebArea);
    }

    #[test]
    fn menus_follow_their_parent() {
        let none = States::default();
        assert_eq!(hint_for_children("menu bar"), Hint::BarItem);
        assert_eq!(hint_for_children("menu"), Hint::MenuItem);
        assert_eq!(hint_for_children("panel"), Hint::None);
        assert_eq!(kind("menu", none, Hint::BarItem), Kind::MenuBarItem);
        assert_eq!(kind("menu", none, Hint::MenuItem), Kind::MenuItem);
        assert_eq!(kind("menu", none, Hint::None), Kind::Menu);
        assert_eq!(kind("popup menu", none, Hint::None), Kind::Menu);
        assert_eq!(kind("menu item", none, Hint::MenuItem), Kind::MenuItem);
        assert_eq!(kind("menu bar", none, Hint::None), Kind::MenuBar);
    }

    #[test]
    fn windows_are_the_frame_like_roles() {
        assert!(is_window_role("frame"));
        assert!(is_window_role("dialog"));
        assert!(!is_window_role("panel"));
    }

    #[test]
    fn secret_fields_are_found_by_role_and_name() {
        assert!(is_secure("password text", None, true));
        assert!(is_secure("entry", Some("Password"), true));
        assert!(is_secure("entry", Some("Enter your PIN"), true));
        assert!(is_secure("entry", Some("One-time code"), true));
        assert!(!is_secure("entry", Some("Pinned notes"), true));
        assert!(!is_secure("entry", Some("Search"), true));
        // A label that says "Password" is not a field.
        assert!(!is_secure("label", Some("Password"), false));
    }

    fn actions(names: &[&str]) -> Vec<(String, String)> {
        names
            .iter()
            .map(|n| (n.to_string(), String::new()))
            .collect()
    }

    #[test]
    fn actions_show_as_lowercase_words() {
        assert_eq!(display_action("Press", ""), Some("press".into()));
        assert_eq!(
            display_action("expand_or_contract", ""),
            Some("expand or contract".into())
        );
        assert_eq!(
            display_action("", "Open the file"),
            Some("open the file".into())
        );
        assert_eq!(display_action("", ""), None);
        assert_eq!(
            action_names(&actions(&["click", "Click", "set_focus"])),
            ["click", "set focus"]
        );
    }

    #[test]
    fn the_usual_action_presses() {
        assert_eq!(preferred_action(&actions(&["setFocus", "click"])), Some(1));
        assert_eq!(preferred_action(&actions(&["Press"])), Some(0));
        assert_eq!(preferred_action(&actions(&["double click"])), Some(0));
        assert_eq!(preferred_action(&actions(&["expand", "scroll"])), None);
        assert_eq!(preferred_action(&[]), None);
    }

    #[test]
    fn actions_are_found_by_the_shown_name() {
        let a = actions(&["click", "set_focus"]);
        assert_eq!(action_index(&a, "set focus"), Some(1));
        assert_eq!(action_index(&a, "SET_FOCUS"), Some(1));
        assert_eq!(action_index(&a, "nothing"), None);
    }

    #[test]
    fn identity_is_stable_and_tells_elements_apart() {
        assert_eq!(identity(":1.5", "/a/1"), identity(":1.5", "/a/1"));
        assert_ne!(identity(":1.5", "/a/1"), identity(":1.5", "/a/2"));
        assert_ne!(identity(":1.5", "/a/1"), identity(":1.6", "/a/1"));
    }

    #[test]
    fn frames_need_a_size() {
        assert_eq!(
            frame((10, 20, 30, 40)),
            Some(Rect {
                x: 10,
                y: 20,
                width: 30,
                height: 40
            })
        );
        assert_eq!(frame((0, 0, 0, 5)), None);
        assert_eq!(frame((-1, -1, -1, -1)), None);
    }

    #[test]
    fn text_and_numbers_read_back() {
        assert_eq!(clipped("abc".into(), 3), "abc");
        assert_eq!(clipped("abc".into(), TEXT_LIMIT + 1), "abc...");
        assert!(truthy("On") && truthy("1") && !truthy("0") && !truthy("off"));
        assert_eq!(number_text(50.0), "50");
        assert_eq!(number_text(0.5), "0.5");
    }
}
