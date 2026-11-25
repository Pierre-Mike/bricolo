//! Where the hub drops down, from its chord or the menu bar: centred on
//! the screen that holds the pointer, flush under the menu bar, three
//! quarters of the screen wide and half of it tall. On a MacBook with a
//! notch the menu bar is as tall as the notch, so the hub hangs from it.

/// The chord that toggles the hub, unless `BRICOLO_UI_CHORD` names another.
pub const CHORD: &str = "cmd+`";

/// The hub's size when it drops down, as fractions of the visible frame's
/// width and height.
pub const SIZE: (f64, f64) = (0.75, 0.5);

/// A rectangle in points. `y` grows upwards, as in AppKit, so `y` is the
/// bottom edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// Edges count as inside: AppKit puts the pointer on a screen's top
    /// row at the frame's top edge.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

/// The chord for a value of `BRICOLO_UI_CHORD`: the default when it is unset,
/// none when it is empty or `off`.
pub fn chord(var: Option<&str>) -> Option<&str> {
    match var.map(str::trim) {
        None => Some(CHORD),
        Some("" | "off") => None,
        Some(chord) => Some(chord),
    }
}

/// The screen that holds the point, as an index into `screens` (their
/// frames); the first screen, the one with the menu bar, when none does.
pub fn screen_at(screens: &[Rect], x: f64, y: f64) -> Option<usize> {
    if screens.is_empty() {
        return None;
    }
    Some(screens.iter().position(|s| s.contains(x, y)).unwrap_or(0))
}

/// The hub's frame on a screen whose visible frame (the screen less the
/// menu bar and the Dock) is `visible`: centred, its top on the visible
/// frame's top, and `size` of the visible frame, in whole points.
pub fn frame(visible: Rect, (width, height): (f64, f64)) -> Rect {
    let width = (visible.width * width.clamp(0.0, 1.0)).round();
    let height = (visible.height * height.clamp(0.0, 1.0)).round();
    Rect {
        x: (visible.x + (visible.width - width) / 2.0).round(),
        y: visible.y + visible.height - height,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn hangs_centred_under_the_menu_bar() {
        // A 14-inch MacBook Pro: 1512 x 982, a 37 pt menu bar, the Dock
        // hidden.
        let visible = rect(0.0, 0.0, 1512.0, 945.0);
        assert_eq!(frame(visible, SIZE), rect(189.0, 472.0, 1134.0, 473.0));
        // A screen left of the main one, with a Dock at the bottom.
        let visible = rect(-1920.0, 80.0, 1920.0, 975.0);
        let f = frame(visible, SIZE);
        assert_eq!(f, rect(-1680.0, 567.0, 1440.0, 488.0));
        assert_eq!(f.y + f.height, visible.y + visible.height);
    }

    #[test]
    fn full_width_and_never_past_the_screen() {
        let visible = rect(0.0, 0.0, 1512.0, 945.0);
        assert_eq!(frame(visible, (1.0, 0.5)), rect(0.0, 472.0, 1512.0, 473.0));
        assert_eq!(frame(visible, (2.0, 3.0)), visible);
    }

    #[test]
    fn picks_the_screen_under_the_pointer() {
        let screens = [
            rect(0.0, 0.0, 1512.0, 982.0),
            rect(-1920.0, 0.0, 1920.0, 1080.0),
        ];
        assert_eq!(screen_at(&screens, 700.0, 500.0), Some(0));
        assert_eq!(screen_at(&screens, -10.0, 1080.0), Some(1));
        // Nowhere, as while a screen goes away: the main screen.
        assert_eq!(screen_at(&screens, 5000.0, 0.0), Some(0));
        assert_eq!(screen_at(&[], 0.0, 0.0), None);
    }

    #[test]
    fn chord_defaults_and_turns_off() {
        assert_eq!(chord(None), Some("cmd+`"));
        assert_eq!(chord(Some("cmd+alt+h ")), Some("cmd+alt+h"));
        assert_eq!(chord(Some("")), None);
        assert_eq!(chord(Some("off")), None);
    }
}
