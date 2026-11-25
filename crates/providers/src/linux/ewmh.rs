//! The pure parts of EWMH and ICCCM: reading window properties, frame
//! extents, and the client messages a pager sends. `x11.rs` does the I/O.

use crate::window::Rect;

/// A text property (`_NET_WM_NAME`, `WM_NAME`): bytes up to the first NUL.
/// `WM_NAME` may be Latin-1; anything that is not UTF-8 is replaced.
pub fn text(value: &[u8]) -> String {
    let end = value.iter().position(|&b| b == 0).unwrap_or(value.len());
    String::from_utf8_lossy(&value[..end]).into_owned()
}

/// `WM_CLASS`: two NUL-terminated strings, the instance ("xterm") and the
/// class ("XTerm"). A missing class repeats the instance.
pub fn wm_class(value: &[u8]) -> (String, String) {
    let mut parts = value.split(|&b| b == 0).map(text);
    let instance = parts.next().unwrap_or_default();
    let class = parts
        .next()
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| instance.clone());
    (instance, class)
}

/// `_NET_FRAME_EXTENTS`: the decoration the window manager adds around a
/// client window, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extents {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

impl Extents {
    /// From the property's four CARDINALs; anything else means none.
    pub fn from_values(values: &[u32]) -> Self {
        match values {
            [left, right, top, bottom, ..] => Self {
                left: *left,
                right: *right,
                top: *top,
                bottom: *bottom,
            },
            _ => Self::default(),
        }
    }

    /// The outer frame, decorations included, of a client window whose
    /// top-left corner sits at `(x, y)` on the root window. This matches
    /// what the macOS backend reports: the whole window, title bar too.
    pub fn outer(&self, x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x: x - self.left as i32,
            y: y - self.top as i32,
            width: width + self.left + self.right,
            height: height + self.top + self.bottom,
        }
    }

    /// The client size that gives an outer frame of `frame`'s size, at
    /// least one pixel each way.
    pub fn inner_size(&self, frame: Rect) -> (u32, u32) {
        let w = frame.width.saturating_sub(self.left + self.right).max(1);
        let h = frame.height.saturating_sub(self.top + self.bottom).max(1);
        (w, h)
    }
}

/// `_NET_MOVERESIZE_WINDOW` gravity: the reference point is the outer
/// frame's top-left corner.
pub const GRAVITY_NORTH_WEST: u32 = 1;
/// "The request comes from a pager": window managers apply it as asked,
/// rather than as an application's suggestion.
pub const SOURCE_PAGER: u32 = 2;

/// The data of a `_NET_MOVERESIZE_WINDOW` client message that moves the
/// outer frame to `frame`'s corner and sizes the client to `width` by
/// `height`. Coordinates travel as CARDINALs, so negative ones wrap.
pub fn moveresize_data(frame: Rect, width: u32, height: u32) -> [u32; 5] {
    // Bits 8 to 11: x, y, width and height are set. Bits 12 and 13: source.
    let flags = GRAVITY_NORTH_WEST | 0xf << 8 | SOURCE_PAGER << 12;
    [flags, frame.x as u32, frame.y as u32, width, height]
}

/// The data of a `_NET_ACTIVE_WINDOW` client message from a pager, with
/// `CurrentTime` as the timestamp.
pub fn activate_data() -> [u32; 5] {
    [SOURCE_PAGER, 0, 0, 0, 0]
}

/// `WM_STATE` value of a minimized window, the data of `WM_CHANGE_STATE`.
pub const ICONIC_STATE: u32 = 3;

/// The data of a `_NET_CLOSE_WINDOW` client message: the current time, and
/// a pager as the source.
pub fn close_data() -> [u32; 5] {
    [0, SOURCE_PAGER, 0, 0, 0]
}

/// `_NET_WM_STATE` actions.
pub const STATE_REMOVE: u32 = 0;

/// The overlap of two rectangles, if they overlap.
pub fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width as i32).min(b.x + b.width as i32);
    let y1 = (a.y + a.height as i32).min(b.y + b.height as i32);
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0,
        y: y0,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    })
}

/// The work area of `desktop` from `_NET_WORKAREA`, which holds four
/// CARDINALs (x, y, width, height) per desktop.
pub fn workarea(values: &[u32], desktop: u32) -> Option<Rect> {
    let start = desktop as usize * 4;
    match values.get(start..start + 4)? {
        [x, y, w, h] => Some(Rect {
            x: *x as i32,
            y: *y as i32,
            width: *w,
            height: *h,
        }),
        _ => None,
    }
}

/// The part of `screen` free of panels and docks: the screen cut down to
/// the work area. A work area spans every monitor, so on a monitor without
/// panels this is the whole monitor.
pub fn visible_frame(screen: Rect, workarea: Option<Rect>) -> Rect {
    workarea
        .and_then(|area| intersect(screen, area))
        .unwrap_or(screen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn text_stops_at_nul_and_replaces_bad_bytes() {
        assert_eq!(text(b"xterm\0"), "xterm");
        assert_eq!(text("café".as_bytes()), "café");
        assert_eq!(text(b"caf\xe9"), "caf\u{fffd}");
        assert_eq!(text(b""), "");
    }

    #[test]
    fn wm_class_has_instance_then_class() {
        assert_eq!(
            wm_class(b"xterm\0XTerm\0"),
            ("xterm".into(), "XTerm".into())
        );
        assert_eq!(
            wm_class(b"navigator\0firefox"),
            ("navigator".into(), "firefox".into())
        );
        assert_eq!(wm_class(b"solo\0"), ("solo".into(), "solo".into()));
        assert_eq!(wm_class(b""), (String::new(), String::new()));
    }

    #[test]
    fn extents_grow_the_client_into_the_frame() {
        // Openbox's default theme: 1 px sides, a 21 px title bar.
        let e = Extents::from_values(&[1, 1, 21, 1]);
        assert_eq!(e.outer(101, 121, 400, 300), rect(100, 100, 402, 322));
        assert_eq!(e.inner_size(rect(100, 100, 402, 322)), (400, 300));
        assert_eq!(e.inner_size(rect(0, 0, 1, 1)), (1, 1));
        assert_eq!(Extents::from_values(&[1, 2]), Extents::default());
    }

    #[test]
    fn moveresize_sets_all_four_fields_as_a_pager() {
        let data = moveresize_data(rect(-10, 20, 0, 0), 640, 480);
        assert_eq!(data[0], 1 | 0xf00 | 0x2000);
        assert_eq!(data[1] as i32, -10);
        assert_eq!(&data[2..], [20, 640, 480]);
        assert_eq!(activate_data()[0], SOURCE_PAGER);
    }

    #[test]
    fn intersect_and_visible_frame() {
        let screen = rect(0, 0, 1920, 1080);
        // A 30 px panel at the top.
        let area = rect(0, 30, 3840, 1050);
        assert_eq!(visible_frame(screen, Some(area)), rect(0, 30, 1920, 1050));
        // Second monitor: the work area only overlaps part of it.
        let right = rect(1920, 0, 1920, 1080);
        assert_eq!(visible_frame(right, Some(area)), rect(1920, 30, 1920, 1050));
        assert_eq!(intersect(screen, rect(1920, 0, 10, 10)), None);
        assert_eq!(visible_frame(screen, None), screen);
    }

    #[test]
    fn workarea_picks_the_desktop() {
        let values = [0, 0, 800, 600, 0, 25, 800, 575];
        assert_eq!(workarea(&values, 1), Some(rect(0, 25, 800, 575)));
        assert_eq!(workarea(&values, 2), None);
    }
}
