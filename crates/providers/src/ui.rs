//! `ui` (Decision 8): plugins and scripts describe their controls (a tile
//! in their hub card), a widget and a few kinds of floating panel as data. The node keeps that state here; the UI
//! process subscribes to it over the control socket and draws it. What the
//! user does there comes back as an [`UiAction`], which updates the state
//! and reaches the owner as a [`ProviderEvent::Ui`].
//!
//! - Each owner (a plugin, or a script as `<name>.lua`) has at most one
//!   tile. `set_tile` replaces it. The hub draws it inside the owner's card.
//! - Each owner has at most one widget, display only, which the hub's
//!   Widgets view draws. `set_widget` replaces it.
//! - Each owner has at most one panel of each kind. Showing a kind again
//!   replaces that panel and keeps its id, so a caption can update many
//!   times a second without piling up.
//! - A toast hides itself after `timeout_ms` (default 4 s). A caption with
//!   a timeout does too. Choice lists and review panels stay until the user
//!   answers or the owner hides them.
//! - `ui` always runs on this node: it is not routable.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent};

/// How long a toast stays when it sets no timeout.
pub const TOAST_MS: u32 = 4000;

/// Limits that keep one plugin from flooding the UI.
const MAX_TITLE: usize = 200;
const MAX_TEXT: usize = 64 * 1024;
const MAX_CONTROLS: usize = 16;
const MAX_CHOICES: usize = 50;
const MAX_ROWS: usize = 16;
const MAX_GAUGES: usize = 8;
const MAX_MARKS: u8 = 32;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Switch {
    pub id: String,
    pub label: String,
    pub on: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Button {
    pub id: String,
    pub label: String,
}

/// A plugin's tile in the hub.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tile {
    pub title: String,
    pub status_text: String,
    pub switches: Vec<Switch>,
    pub buttons: Vec<Button>,
}

/// One labelled value of a widget, like `Paris` and `20:05`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WidgetRow {
    pub label: String,
    pub value: String,
}

/// A display-only widget in the hub's Widgets view: no switches or
/// buttons, which belong in the tile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Widget {
    pub title: String,
    /// Bars drawn above the rows; empty for a plain widget.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gauges: Vec<Gauge>,
    pub rows: Vec<WidgetRow>,
}

/// A bar across a widget: `label` and `value` above it, `note` under it,
/// split into `marks` segments, filled to `fill` percent, with a line at
/// `now` percent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gauge {
    pub label: String,
    pub value: String,
    pub fill: u8,
    pub marks: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<u8>,
    #[serde(default)]
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PanelKind {
    /// Text near the bottom of the screen, like live dictation.
    Caption,
    /// A list to pick one entry from, with the keyboard or the mouse.
    ChoiceList,
    /// A short message that goes away by itself.
    Toast,
    /// Editable text the user accepts or cancels.
    Review,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Panel {
    pub kind: PanelKind,
    pub title: String,
    /// The caption, the toast, or the text to review.
    pub text: String,
    /// The entries of a choice list.
    pub choices: Vec<String>,
    /// Hide after this long. Toasts default to [`TOAST_MS`].
    pub timeout_ms: Option<u32>,
}

/// A panel on screen, with its id and owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ShownPanel {
    pub id: String,
    pub owner: String,
    /// Grows with each show, so the UI can draw the newest panel of a kind.
    pub seq: u64,
    #[serde(flatten)]
    pub panel: Panel,
}

/// What the UI draws.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UiSnapshot {
    /// Tiles by owner.
    pub tiles: BTreeMap<String, Tile>,
    /// Widgets by owner.
    pub widgets: BTreeMap<String, Widget>,
    pub panels: Vec<ShownPanel>,
}

/// What reaches the owner when the user acts on its tile or panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum UiEvent {
    /// A tile switch was toggled; `on` is its new value.
    Switch {
        id: String,
        on: bool,
    },
    Button {
        id: String,
    },
    /// An entry of a choice list was picked, or the list was dismissed
    /// (`index` is `None`).
    Choice {
        panel: String,
        index: Option<u32>,
    },
    /// A review panel was accepted, with the edited text, or cancelled.
    Review {
        panel: String,
        accepted: bool,
        text: String,
    },
}

/// What the UI sends back: a [`UiEvent`], and for tile events the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UiAction {
    Switch {
        owner: String,
        id: String,
        on: bool,
    },
    Button {
        owner: String,
        id: String,
    },
    Choice {
        panel: String,
        index: Option<u32>,
    },
    Review {
        panel: String,
        accepted: bool,
        text: String,
    },
}

pub trait UiProvider: Send + Sync {
    fn set_tile(&self, caller: &Caller, tile: Tile) -> Result<(), CallError>;
    fn clear_tile(&self, caller: &Caller) -> Result<(), CallError>;
    fn set_widget(&self, caller: &Caller, widget: Widget) -> Result<(), CallError>;
    fn clear_widget(&self, caller: &Caller) -> Result<(), CallError>;
    /// Shows `panel` and returns its id.
    fn show_panel(&self, caller: &Caller, panel: Panel) -> Result<String, CallError>;
    /// Hides one of the caller's panels. Unknown ids are ignored: a toast
    /// may already be gone.
    fn hide_panel(&self, caller: &Caller, id: &str) -> Result<(), CallError>;
    /// The current tiles, widgets and panels.
    fn snapshot(&self) -> UiSnapshot;
    /// Changes each time the tiles, widgets or panels change.
    fn subscribe(&self) -> watch::Receiver<u64>;
    /// Applies what the user did and delivers the event to the owner.
    fn act(&self, action: UiAction) -> Result<(), CallError>;
    /// Drops the owner's tile, widget and panels. Called when it stops.
    fn release(&self, owner: &str);
}

#[derive(Default)]
struct State {
    tiles: BTreeMap<String, Tile>,
    widgets: BTreeMap<String, Widget>,
    panels: Vec<ShownPanel>,
}

pub struct LocalUi {
    state: Arc<Mutex<State>>,
    changed: Arc<watch::Sender<u64>>,
    sink: EventSink,
    next: AtomicU64,
    handle: Handle,
}

impl LocalUi {
    /// Must be called inside a Tokio runtime; panel timeouts run on it.
    pub fn new(sink: EventSink) -> Self {
        Self {
            state: Arc::default(),
            changed: Arc::new(watch::channel(0).0),
            sink,
            next: AtomicU64::new(1),
            handle: Handle::current(),
        }
    }

    fn bump(&self) {
        self.changed.send_modify(|v| *v += 1);
    }

    /// Hides panel `id` after `ms`, unless it was shown again since.
    fn expire(&self, id: String, seq: u64, ms: u32) {
        let state = self.state.clone();
        let changed = self.changed.clone();
        self.handle.spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms.into())).await;
            let mut s = state.lock().unwrap();
            let before = s.panels.len();
            s.panels.retain(|p| !(p.id == id && p.seq == seq));
            if s.panels.len() != before {
                changed.send_modify(|v| *v += 1);
            }
        });
    }

    /// Removes the panel and returns its owner, if it has `kind`.
    fn take_panel(&self, id: &str, kind: PanelKind) -> Result<ShownPanel, CallError> {
        let mut s = self.state.lock().unwrap();
        let at = s
            .panels
            .iter()
            .position(|p| p.id == id && p.panel.kind == kind)
            .ok_or_else(|| CallError::Failed(format!("no {kind:?} panel {id}")))?;
        Ok(s.panels.remove(at))
    }
}

fn check_len(what: &str, text: &str, max: usize) -> Result<(), CallError> {
    if text.len() > max {
        return Err(CallError::Failed(format!(
            "{what} is longer than {max} bytes"
        )));
    }
    Ok(())
}

fn check_tile(tile: &Tile) -> Result<(), CallError> {
    check_len("the title", &tile.title, MAX_TITLE)?;
    check_len("the status text", &tile.status_text, MAX_TITLE)?;
    if tile.switches.len() > MAX_CONTROLS || tile.buttons.len() > MAX_CONTROLS {
        return Err(CallError::Failed(format!(
            "a tile has at most {MAX_CONTROLS} switches and {MAX_CONTROLS} buttons"
        )));
    }
    let mut ids: Vec<&str> = tile.switches.iter().map(|s| s.id.as_str()).collect();
    ids.extend(tile.buttons.iter().map(|b| b.id.as_str()));
    for (s, label) in tile
        .switches
        .iter()
        .map(|s| (&s.id, &s.label))
        .chain(tile.buttons.iter().map(|b| (&b.id, &b.label)))
    {
        if s.is_empty() {
            return Err(CallError::Failed("a switch or button needs an id".into()));
        }
        check_len("a label", label, MAX_TITLE)?;
    }
    let count = ids.len();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != count {
        return Err(CallError::Failed(
            "switch and button ids must be unique within a tile".into(),
        ));
    }
    Ok(())
}

fn check_widget(widget: &Widget) -> Result<(), CallError> {
    check_len("the title", &widget.title, MAX_TITLE)?;
    if widget.rows.len() > MAX_ROWS {
        return Err(CallError::Failed(format!(
            "a widget has at most {MAX_ROWS} rows"
        )));
    }
    for row in &widget.rows {
        check_len("a row label", &row.label, MAX_TITLE)?;
        check_len("a row value", &row.value, MAX_TITLE)?;
    }
    if widget.gauges.len() > MAX_GAUGES {
        return Err(CallError::Failed(format!(
            "a widget has at most {MAX_GAUGES} gauges"
        )));
    }
    for g in &widget.gauges {
        check_len("a gauge label", &g.label, MAX_TITLE)?;
        check_len("a gauge value", &g.value, MAX_TITLE)?;
        check_len("a gauge note", &g.note, MAX_TITLE)?;
        if g.fill > 100 || g.now.is_some_and(|n| n > 100) {
            return Err(CallError::Failed(
                "a gauge's fill and now are 0 to 100".into(),
            ));
        }
        if !(1..=MAX_MARKS).contains(&g.marks) {
            return Err(CallError::Failed(format!(
                "a gauge has 1 to {MAX_MARKS} marks"
            )));
        }
    }
    Ok(())
}

fn check_panel(panel: &Panel) -> Result<(), CallError> {
    check_len("the title", &panel.title, MAX_TITLE)?;
    check_len("the text", &panel.text, MAX_TEXT)?;
    if panel.choices.len() > MAX_CHOICES {
        return Err(CallError::Failed(format!(
            "a choice list has at most {MAX_CHOICES} entries"
        )));
    }
    for choice in &panel.choices {
        check_len("a choice", choice, MAX_TITLE)?;
    }
    if panel.kind == PanelKind::ChoiceList && panel.choices.is_empty() {
        return Err(CallError::Failed("a choice list needs choices".into()));
    }
    Ok(())
}

impl UiProvider for LocalUi {
    fn set_tile(&self, caller: &Caller, tile: Tile) -> Result<(), CallError> {
        check_tile(&tile)?;
        let mut s = self.state.lock().unwrap();
        if s.tiles.get(&caller.plugin) != Some(&tile) {
            s.tiles.insert(caller.plugin.clone(), tile);
            drop(s);
            self.bump();
        }
        Ok(())
    }

    fn clear_tile(&self, caller: &Caller) -> Result<(), CallError> {
        let removed = self.state.lock().unwrap().tiles.remove(&caller.plugin);
        if removed.is_some() {
            self.bump();
        }
        Ok(())
    }

    fn set_widget(&self, caller: &Caller, widget: Widget) -> Result<(), CallError> {
        check_widget(&widget)?;
        let mut s = self.state.lock().unwrap();
        if s.widgets.get(&caller.plugin) != Some(&widget) {
            s.widgets.insert(caller.plugin.clone(), widget);
            drop(s);
            self.bump();
        }
        Ok(())
    }

    fn clear_widget(&self, caller: &Caller) -> Result<(), CallError> {
        let removed = self.state.lock().unwrap().widgets.remove(&caller.plugin);
        if removed.is_some() {
            self.bump();
        }
        Ok(())
    }

    fn show_panel(&self, caller: &Caller, panel: Panel) -> Result<String, CallError> {
        check_panel(&panel)?;
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        let timeout = match panel.kind {
            PanelKind::Toast => Some(panel.timeout_ms.unwrap_or(TOAST_MS)),
            _ => panel.timeout_ms,
        };
        let id = {
            let mut s = self.state.lock().unwrap();
            let same = |p: &&mut ShownPanel| p.owner == caller.plugin && p.panel.kind == panel.kind;
            match s.panels.iter_mut().find(same) {
                Some(shown) => {
                    shown.panel = panel;
                    shown.seq = seq;
                    shown.id.clone()
                }
                None => {
                    let id = format!("p{seq}");
                    s.panels.push(ShownPanel {
                        id: id.clone(),
                        owner: caller.plugin.clone(),
                        seq,
                        panel,
                    });
                    id
                }
            }
        };
        self.bump();
        if let Some(ms) = timeout {
            self.expire(id.clone(), seq, ms);
        }
        Ok(id)
    }

    fn hide_panel(&self, caller: &Caller, id: &str) -> Result<(), CallError> {
        let mut s = self.state.lock().unwrap();
        let before = s.panels.len();
        s.panels
            .retain(|p| !(p.id == id && p.owner == caller.plugin));
        if s.panels.len() != before {
            drop(s);
            self.bump();
        }
        Ok(())
    }

    fn snapshot(&self) -> UiSnapshot {
        let s = self.state.lock().unwrap();
        UiSnapshot {
            tiles: s.tiles.clone(),
            widgets: s.widgets.clone(),
            panels: s.panels.clone(),
        }
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn act(&self, action: UiAction) -> Result<(), CallError> {
        let (owner, event) = match action {
            UiAction::Switch { owner, id, on } => {
                let mut s = self.state.lock().unwrap();
                let switch = s
                    .tiles
                    .get_mut(&owner)
                    .and_then(|t| t.switches.iter_mut().find(|w| w.id == id))
                    .ok_or_else(|| CallError::Failed(format!("{owner} has no switch {id}")))?;
                switch.on = on;
                (owner, UiEvent::Switch { id, on })
            }
            UiAction::Button { owner, id } => {
                let s = self.state.lock().unwrap();
                let known = s
                    .tiles
                    .get(&owner)
                    .is_some_and(|t| t.buttons.iter().any(|b| b.id == id));
                if !known {
                    return Err(CallError::Failed(format!("{owner} has no button {id}")));
                }
                (owner, UiEvent::Button { id })
            }
            UiAction::Choice { panel, index } => {
                let shown = self.take_panel(&panel, PanelKind::ChoiceList)?;
                if let Some(i) = index
                    && i as usize >= shown.panel.choices.len()
                {
                    self.state.lock().unwrap().panels.push(shown);
                    return Err(CallError::Failed(format!("no choice {i}")));
                }
                (shown.owner, UiEvent::Choice { panel, index })
            }
            UiAction::Review {
                panel,
                accepted,
                text,
            } => {
                let shown = self.take_panel(&panel, PanelKind::Review)?;
                (
                    shown.owner,
                    UiEvent::Review {
                        panel,
                        accepted,
                        text,
                    },
                )
            }
        };
        self.bump();
        (self.sink)(&owner, ProviderEvent::Ui(event));
        Ok(())
    }

    fn release(&self, owner: &str) {
        let mut s = self.state.lock().unwrap();
        let had_tile = s.tiles.remove(owner).is_some();
        let had_widget = s.widgets.remove(owner).is_some();
        let before = s.panels.len();
        s.panels.retain(|p| p.owner != owner);
        if had_tile || had_widget || s.panels.len() != before {
            drop(s);
            self.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Seen = Arc<Mutex<Vec<(String, ProviderEvent)>>>;

    fn ui() -> (LocalUi, Seen) {
        let seen: Seen = Arc::default();
        let s = seen.clone();
        let sink: EventSink = Arc::new(move |owner: &str, e: ProviderEvent| {
            s.lock().unwrap().push((owner.to_string(), e));
        });
        (LocalUi::new(sink), seen)
    }

    fn tile() -> Tile {
        Tile {
            title: "Dictation".into(),
            status_text: "idle".into(),
            switches: vec![Switch {
                id: "review".into(),
                label: "Review".into(),
                on: true,
            }],
            buttons: vec![Button {
                id: "go".into(),
                label: "Go".into(),
            }],
        }
    }

    fn panel(kind: PanelKind, text: &str) -> Panel {
        Panel {
            kind,
            title: String::new(),
            text: text.into(),
            choices: vec![],
            timeout_ms: None,
        }
    }

    #[tokio::test]
    async fn tiles_and_switches_round_trip() {
        let (ui, seen) = ui();
        let mut changes = ui.subscribe();
        let caller = Caller::new("dict");
        ui.set_tile(&caller, tile()).unwrap();
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        // Setting the same tile again changes nothing.
        ui.set_tile(&caller, tile()).unwrap();
        assert!(!changes.has_changed().unwrap());
        ui.act(UiAction::Switch {
            owner: "dict".into(),
            id: "review".into(),
            on: false,
        })
        .unwrap();
        assert!(!ui.snapshot().tiles["dict"].switches[0].on);
        ui.act(UiAction::Button {
            owner: "dict".into(),
            id: "go".into(),
        })
        .unwrap();
        assert!(
            ui.act(UiAction::Button {
                owner: "dict".into(),
                id: "nope".into()
            })
            .is_err()
        );
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (
                    "dict".to_string(),
                    ProviderEvent::Ui(UiEvent::Switch {
                        id: "review".into(),
                        on: false
                    })
                ),
                (
                    "dict".to_string(),
                    ProviderEvent::Ui(UiEvent::Button { id: "go".into() })
                ),
            ]
        );
        ui.clear_tile(&caller).unwrap();
        assert!(ui.snapshot().tiles.is_empty());
    }

    fn widget() -> Widget {
        Widget {
            title: "World clock".into(),
            rows: vec![WidgetRow {
                label: "Paris".into(),
                value: "20:05".into(),
            }],
            ..Widget::default()
        }
    }

    fn gauge() -> Gauge {
        Gauge {
            label: "Session".into(),
            value: "5%".into(),
            fill: 5,
            marks: 5,
            now: Some(44),
            note: "Resets in 2 h 48 min".into(),
        }
    }

    #[tokio::test]
    async fn gauges_ride_on_the_widget() {
        let (ui, _) = ui();
        let caller = Caller::new("usage");
        let mut w = widget();
        w.gauges = vec![gauge()];
        ui.set_widget(&caller, w.clone()).unwrap();
        assert_eq!(ui.snapshot().widgets["usage"], w);
        let json = serde_json::to_value(&w).unwrap();
        assert_eq!(json["gauges"][0]["now"], 44);
        let plain = serde_json::to_value(widget()).unwrap();
        assert!(
            plain.get("gauges").is_none(),
            "plain widgets stay as they were"
        );
    }

    #[tokio::test]
    async fn bad_gauges_are_refused() {
        let (ui, _) = ui();
        let caller = Caller::new("p");
        for bad in [
            Gauge {
                fill: 101,
                ..gauge()
            },
            Gauge {
                now: Some(101),
                ..gauge()
            },
            Gauge {
                marks: 0,
                ..gauge()
            },
            Gauge {
                marks: MAX_MARKS + 1,
                ..gauge()
            },
            Gauge {
                note: "x".repeat(MAX_TITLE + 1),
                ..gauge()
            },
        ] {
            let mut w = widget();
            w.gauges = vec![bad];
            assert!(ui.set_widget(&caller, w).is_err());
        }
        let mut w = widget();
        w.gauges = vec![gauge(); MAX_GAUGES + 1];
        assert!(ui.set_widget(&caller, w).is_err());
    }

    #[tokio::test]
    async fn widgets_are_kept_apart_from_tiles_and_released() {
        let (ui, _) = ui();
        let mut changes = ui.subscribe();
        let caller = Caller::new("clock");
        ui.set_widget(&caller, widget()).unwrap();
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        ui.set_widget(&caller, widget()).unwrap();
        assert!(
            !changes.has_changed().unwrap(),
            "the same widget changes nothing"
        );
        let snap = ui.snapshot();
        assert_eq!(snap.widgets["clock"], widget());
        assert!(snap.tiles.is_empty(), "a widget is not a tile");
        ui.clear_widget(&caller).unwrap();
        assert!(ui.snapshot().widgets.is_empty());
        ui.set_widget(&caller, widget()).unwrap();
        ui.release("clock");
        assert!(ui.snapshot().widgets.is_empty());
    }

    #[tokio::test]
    async fn bad_widgets_are_refused() {
        let (ui, _) = ui();
        let caller = Caller::new("p");
        let mut w = widget();
        w.rows = vec![WidgetRow::default(); MAX_ROWS + 1];
        assert!(ui.set_widget(&caller, w).is_err());
        let mut w = widget();
        w.rows[0].value = "x".repeat(MAX_TITLE + 1);
        assert!(ui.set_widget(&caller, w).is_err());
    }

    #[tokio::test]
    async fn bad_tiles_are_refused() {
        let (ui, _) = ui();
        let caller = Caller::new("p");
        let mut t = tile();
        t.buttons[0].id = "review".into();
        assert!(ui.set_tile(&caller, t).is_err());
        let mut t = tile();
        t.title = "x".repeat(MAX_TITLE + 1);
        assert!(ui.set_tile(&caller, t).is_err());
        assert!(
            ui.show_panel(&caller, panel(PanelKind::ChoiceList, ""))
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_kind_shown_again_replaces_its_panel() {
        let (ui, _) = ui();
        let caller = Caller::new("p");
        let a = ui
            .show_panel(&caller, panel(PanelKind::Caption, "one"))
            .unwrap();
        let b = ui
            .show_panel(&caller, panel(PanelKind::Caption, "two"))
            .unwrap();
        assert_eq!(a, b);
        let other = ui
            .show_panel(&Caller::new("q"), panel(PanelKind::Caption, "q"))
            .unwrap();
        assert_ne!(a, other);
        let snap = ui.snapshot();
        assert_eq!(snap.panels.len(), 2);
        assert_eq!(snap.panels[0].panel.text, "two");
        // Only the owner can hide a panel.
        ui.hide_panel(&Caller::new("q"), &a).unwrap();
        assert_eq!(ui.snapshot().panels.len(), 2);
        ui.hide_panel(&caller, &a).unwrap();
        assert_eq!(ui.snapshot().panels.len(), 1);
        ui.release("q");
        assert!(ui.snapshot().panels.is_empty());
    }

    #[tokio::test]
    async fn choices_and_reviews_close_and_reach_the_owner() {
        let (ui, seen) = ui();
        let caller = Caller::new("p");
        let mut list = panel(PanelKind::ChoiceList, "");
        list.choices = vec!["a".into(), "b".into()];
        let id = ui.show_panel(&caller, list).unwrap();
        assert!(
            ui.act(UiAction::Choice {
                panel: id.clone(),
                index: Some(2)
            })
            .is_err()
        );
        assert_eq!(ui.snapshot().panels.len(), 1, "a bad index keeps the list");
        ui.act(UiAction::Choice {
            panel: id.clone(),
            index: Some(1),
        })
        .unwrap();
        assert!(ui.snapshot().panels.is_empty());
        let review = ui
            .show_panel(&caller, panel(PanelKind::Review, "draft"))
            .unwrap();
        ui.act(UiAction::Review {
            panel: review.clone(),
            accepted: true,
            text: "edited".into(),
        })
        .unwrap();
        // A second answer finds no panel.
        assert!(
            ui.act(UiAction::Review {
                panel: review.clone(),
                accepted: false,
                text: String::new()
            })
            .is_err()
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].1,
            ProviderEvent::Ui(UiEvent::Choice {
                panel: id,
                index: Some(1)
            })
        );
        assert_eq!(
            seen[1].1,
            ProviderEvent::Ui(UiEvent::Review {
                panel: review,
                accepted: true,
                text: "edited".into()
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn toasts_hide_themselves() {
        let (ui, _) = ui();
        let caller = Caller::new("p");
        ui.show_panel(&caller, panel(PanelKind::Toast, "saved"))
            .unwrap();
        let mut caption = panel(PanelKind::Caption, "hi");
        caption.timeout_ms = Some(10_000);
        ui.show_panel(&caller, caption).unwrap();
        tokio::time::sleep(Duration::from_millis(u64::from(TOAST_MS) + 10)).await;
        let snap = ui.snapshot();
        assert_eq!(snap.panels.len(), 1);
        assert_eq!(snap.panels[0].panel.kind, PanelKind::Caption);
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(ui.snapshot().panels.is_empty());
    }

    #[test]
    fn actions_parse_from_json() {
        let a: UiAction =
            serde_json::from_str(r#"{"kind":"switch","owner":"p","id":"x","on":true}"#).unwrap();
        assert_eq!(
            a,
            UiAction::Switch {
                owner: "p".into(),
                id: "x".into(),
                on: true
            }
        );
        let a: UiAction =
            serde_json::from_str(r#"{"kind":"choice","panel":"p1","index":null}"#).unwrap();
        assert_eq!(
            a,
            UiAction::Choice {
                panel: "p1".into(),
                index: None
            }
        );
    }
}
