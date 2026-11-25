//! Tiles the focused window into the visible frame of the screen it is on:
//! cmd+alt+left and cmd+alt+right for the halves, cmd+alt+up to fill.
//! Ported from the plugin-host spike.

// The generated `event` bindings take more than seven arguments.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../wit",
    world: "bricolo:stdlib/plugin",
});

use bricolo::stdlib::hotkey;
use bricolo::stdlib::log::{self, Level};
use bricolo::stdlib::window::{self, Rect, Screen};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    LeftHalf,
    RightHalf,
    Fill,
}

impl Layout {
    const ALL: [Layout; 3] = [Layout::LeftHalf, Layout::RightHalf, Layout::Fill];

    /// The binding id, also used in logs.
    fn id(self) -> &'static str {
        match self {
            Layout::LeftHalf => "left-half",
            Layout::RightHalf => "right-half",
            Layout::Fill => "fill",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.id() == id)
    }

    fn chord(self) -> &'static str {
        match self {
            Layout::LeftHalf => "cmd+alt+left",
            Layout::RightHalf => "cmd+alt+right",
            Layout::Fill => "cmd+alt+up",
        }
    }

    /// The rect for a window tiled inside `area`. The left half gets the
    /// smaller half when the width is odd, so the halves never overlap.
    fn target(self, area: Rect) -> Rect {
        let left = area.width / 2;
        match self {
            Layout::LeftHalf => Rect {
                width: left,
                ..area
            },
            Layout::RightHalf => Rect {
                x: area.x + left as i32,
                width: area.width - left,
                ..area
            },
            Layout::Fill => area,
        }
    }
}

fn overlap(a: &Rect, b: &Rect) -> u64 {
    let w = (a.x + a.width as i32).min(b.x + b.width as i32) - a.x.max(b.x);
    let h = (a.y + a.height as i32).min(b.y + b.height as i32) - a.y.max(b.y);
    if w <= 0 || h <= 0 {
        0
    } else {
        w as u64 * h as u64
    }
}

/// The screen the window overlaps most; when it overlaps none, the main
/// screen, then the first one.
fn screen_for<'a>(window: &Rect, screens: &'a [Screen]) -> Option<&'a Screen> {
    screens
        .iter()
        .map(|s| (overlap(&s.frame, window), s))
        .filter(|(area, _)| *area > 0)
        .max_by_key(|(area, _)| *area)
        .map(|(_, s)| s)
        .or_else(|| screens.iter().find(|s| s.main))
        .or_else(|| screens.first())
}

async fn tile(layout: Layout) -> Result<String, String> {
    let window = window::focused()
        .await
        .map_err(|e| format!("focused: {e:?}"))?
        .ok_or("no focused window")?;
    let screens = window::screens()
        .await
        .map_err(|e| format!("screens: {e:?}"))?;
    let screen = screen_for(&window.frame, &screens).ok_or("no screens")?;
    let target = layout.target(screen.visible_frame);
    window::set_frame(window.id, target)
        .await
        .map_err(|e| format!("set-frame: {e:?}"))?;
    Ok(format!(
        "{} window {} -> {},{} {}x{}",
        window.app, window.id, target.x, target.y, target.width, target.height
    ))
}

struct Tiler;

impl Guest for Tiler {
    async fn init(_config: String) -> Result<(), String> {
        for layout in Layout::ALL {
            hotkey::bind(layout.id().into(), layout.chord().into())
                .await
                .map_err(|e| format!("cannot bind {}: {e:?}", layout.chord()))?;
        }
        log::write(Level::Info, &format!("bound {} layouts", Layout::ALL.len()));
        Ok(())
    }

    async fn on_event(event: Event) {
        let Event::Hotkey(id) = event else {
            return;
        };
        let Some(layout) = Layout::from_id(&id) else {
            log::write(Level::Warn, &format!("no layout for hotkey {id}"));
            return;
        };
        match tile(layout).await {
            Ok(done) => log::write(Level::Info, &format!("{id}: {done}")),
            Err(e) => log::write(Level::Warn, &format!("{id}: {e}")),
        }
    }

    async fn dispose() {}
}

export!(Tiler);

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 33,
        width: 1512,
        height: 869,
    };

    fn screen(x: i32, width: u32, main: bool) -> Screen {
        let frame = Rect {
            x,
            y: 0,
            width,
            height: 1000,
        };
        Screen {
            id: 0,
            frame,
            visible_frame: Rect {
                y: 25,
                height: 975,
                ..frame
            },
            main,
        }
    }

    #[test]
    fn halves_split_the_visible_frame_without_overlap() {
        assert_eq!(Layout::LeftHalf.target(AREA).width, 756);
        assert_eq!(Layout::RightHalf.target(AREA).x, 756);
        let odd = Rect { width: 101, ..AREA };
        let (l, r) = (Layout::LeftHalf.target(odd), Layout::RightHalf.target(odd));
        assert_eq!(l.x + l.width as i32, r.x);
        assert_eq!(l.width + r.width, 101);
        let fill = Layout::Fill.target(AREA);
        assert_eq!(
            (fill.x, fill.y, fill.width, fill.height),
            (0, 33, 1512, 869)
        );
    }

    #[test]
    fn picks_the_screen_holding_most_of_the_window() {
        let screens = [screen(0, 1500, true), screen(1500, 1920, false)];
        let window = Rect {
            x: 1400,
            y: 100,
            width: 800,
            height: 400,
        };
        assert_eq!(screen_for(&window, &screens).unwrap().frame.x, 1500);
        let lost = Rect {
            x: 9000,
            y: 9000,
            width: 10,
            height: 10,
        };
        let screens = [screen(1500, 1920, false), screen(0, 1500, true)];
        assert!(screen_for(&lost, &screens).unwrap().main);
        assert!(screen_for(&lost, &[]).is_none());
    }

    #[test]
    fn ids_round_trip() {
        for layout in Layout::ALL {
            assert_eq!(Layout::from_id(layout.id()), Some(layout));
        }
        assert_eq!(Layout::from_id("top-half"), None);
    }
}
