//! Where the floating panels go: on the screen the user is working on,
//! the one with the focused window, so a caption shows where the user is
//! looking when there are several screens. Captions and reviews sit at the
//! bottom centre, a toast above the caption's place, a choice list in the
//! upper third.

use crate::dropdown::Rect;

/// Width and height in points.
pub fn size(kind: &str) -> (f64, f64) {
    match kind {
        "caption" => (760.0, 120.0),
        "toast" => (420.0, 72.0),
        "choice-list" => (440.0, 360.0),
        // A bar along the bottom: about four lines of text.
        "review" => (760.0, 200.0),
        _ => (600.0, 340.0),
    }
}

/// Gap between a bottom panel and the bottom of the visible frame.
const MARGIN: f64 = 48.0;

/// The panel's frame on a screen whose visible frame (the screen less the
/// menu bar and the Dock) is `visible`, in whole points. `y` grows
/// upwards, as in AppKit.
pub fn frame(kind: &str, visible: Rect) -> Rect {
    let (width, height) = size(kind);
    let y = match kind {
        "caption" | "review" => visible.y + MARGIN,
        "toast" => {
            let below = size("caption").1.max(size("review").1);
            visible.y + MARGIN + below + 16.0
        }
        // The top edge a quarter of the way down, less a quarter of the
        // panel's height.
        _ => visible.y + visible.height - (visible.height / 4.0 - height / 4.0) - height,
    };
    Rect {
        x: (visible.x + (visible.width - width) / 2.0).round(),
        y: y.round(),
        width,
        height,
    }
}

/// The screen to show panels on, as an index into `screens` (their
/// frames): `focused`, the screen with the focused window, when there is
/// one, else the screen with the pointer, else the first screen.
pub fn screen(screens: &[Rect], focused: Option<Rect>, pointer: (f64, f64)) -> Option<usize> {
    if let Some(f) = focused
        && let Some(i) = screens.iter().position(|s| *s == f)
    {
        return Some(i);
    }
    crate::dropdown::screen_at(screens, pointer.0, pointer.1)
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

    // A laptop at the origin and a 1920 x 1080 screen to its right.
    const LAPTOP: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1512.0,
        height: 982.0,
    };
    const RIGHT: Rect = Rect {
        x: 1512.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };

    #[test]
    fn the_focused_screen_wins_over_the_pointer() {
        let screens = [LAPTOP, RIGHT];
        assert_eq!(screen(&screens, Some(RIGHT), (10.0, 10.0)), Some(1));
        assert_eq!(screen(&screens, Some(LAPTOP), (2000.0, 10.0)), Some(0));
    }

    #[test]
    fn without_a_focused_screen_the_pointer_decides() {
        let screens = [LAPTOP, RIGHT];
        assert_eq!(screen(&screens, None, (2000.0, 10.0)), Some(1));
        let gone = rect(9999.0, 0.0, 10.0, 10.0);
        assert_eq!(screen(&screens, Some(gone), (2000.0, 10.0)), Some(1));
        assert_eq!(screen(&[], None, (0.0, 0.0)), None);
    }

    #[test]
    fn a_caption_sits_at_the_bottom_centre_of_its_screen() {
        // The second screen's visible frame, less a 25 pt menu bar.
        let visible = rect(1512.0, 0.0, 1920.0, 1055.0);
        let f = frame("caption", visible);
        assert_eq!(f, rect(1512.0 + 580.0, 48.0, 760.0, 120.0));
    }

    #[test]
    fn a_toast_sits_above_the_caption_place() {
        let f = frame("toast", rect(0.0, 0.0, 1512.0, 945.0));
        assert_eq!(f.y, 48.0 + 200.0 + 16.0);
        assert_eq!(f.x, 546.0);
    }

    #[test]
    fn a_choice_list_sits_in_the_upper_third() {
        let f = frame("choice-list", rect(0.0, 0.0, 1000.0, 800.0));
        // Top edge 200 - 90 = 110 pt below the top.
        assert_eq!(f.y + f.height, 800.0 - 110.0);
    }
}
