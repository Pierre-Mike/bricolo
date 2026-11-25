//! `screen`: PNG pictures of this device's screens. The macOS backend runs
//! `/usr/sbin/screencapture` (`macos/screen.rs`). On Linux (`linux/screen.rs`)
//! an X11 session reads the root window with `GetImage` and encodes the PNG
//! itself; a Wayland session runs `grim` when it is on `PATH`; without
//! `grim`, or without a display, it answers `unavailable`. Elsewhere it
//! answers `unavailable`.
//!
//! The pixel conversion, the clipping to the screen and the `grim`
//! arguments live here, free of any OS code, so the tests run everywhere.
//!
//! `screen` never runs on a peer: a picture of this device's screen is
//! private to it.

use std::path::Path;

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;
use crate::window::Rect;

#[async_trait]
pub trait ScreenProvider: Send + Sync {
    /// Returns the path of the PNG it wrote.
    async fn capture(&self, caller: &Caller, area: Option<Rect>) -> Result<String, CallError>;
}

/// No screen capture on this node.
pub struct NoScreen;

#[async_trait]
impl ScreenProvider for NoScreen {
    async fn capture(&self, _: &Caller, _: Option<Rect>) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
}

/// `Failed("the area is empty")` when `width` or `height` is 0.
pub fn check_area(area: &Rect) -> Result<(), CallError> {
    if area.width == 0 || area.height == 0 {
        return Err(CallError::Failed("the area is empty".into()));
    }
    Ok(())
}

/// The arguments for macOS `/usr/sbin/screencapture`: no shutter sound
/// (`-x`), then `-m` (main screen only) or `-R x,y,w,h`, then `path`.
pub fn screencapture_args(area: Option<Rect>, path: &Path) -> Vec<String> {
    let mut args = vec!["-x".to_string()];
    match area {
        None => args.push("-m".into()),
        Some(a) => {
            args.push("-R".into());
            args.push(format!("{},{},{},{}", a.x, a.y, a.width, a.height));
        }
    }
    args.push(path.display().to_string());
    args
}

/// The arguments for `grim` on Wayland: `-g "X,Y WxH"` for an area, then
/// `path`. Without an area it takes every output.
pub fn grim_args(area: Option<Rect>, path: &Path) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(a) = area {
        args.push("-g".into());
        args.push(format!("{},{} {}x{}", a.x, a.y, a.width, a.height));
    }
    args.push(path.display().to_string());
    args
}

/// `area` cut down to a screen of `width` by `height` pixels with its corner
/// at 0,0. `Failed` when nothing of it is on the screen.
pub fn clip_area(area: Rect, width: u32, height: u32) -> Result<Rect, CallError> {
    let left = i64::from(area.x).max(0);
    let top = i64::from(area.y).max(0);
    let right = (i64::from(area.x) + i64::from(area.width)).min(i64::from(width));
    let bottom = (i64::from(area.y) + i64::from(area.height)).min(i64::from(height));
    if right <= left || bottom <= top {
        return Err(CallError::Failed("the area is outside the screen".into()));
    }
    // All four fit: they lie between 0 and the screen size.
    Ok(Rect {
        x: left as i32,
        y: top as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

/// How a TrueColor image stores its pixels, as an X server describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelFormat {
    /// 24 or 32; anything else is refused.
    pub bits_per_pixel: u8,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    /// The image byte order: most significant byte first.
    pub big_endian: bool,
}

/// One channel's mask: where it starts and how many bits it has.
fn channel(mask: u32) -> Result<(u32, u32), CallError> {
    if mask == 0 {
        return Err(CallError::Failed(
            "the visual has an empty color mask".into(),
        ));
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).trailing_ones();
    Ok((shift, bits))
}

/// Scales a `bits`-wide value to 0..=255.
fn scale(value: u32, bits: u32) -> u8 {
    match bits {
        8 => value as u8,
        b if b > 8 => (value >> (b - 8)) as u8,
        b => ((value * 255) / ((1 << b) - 1)) as u8,
    }
}

/// Converts the rows of a `ZPixmap` image, `width` by `height` pixels,
/// into RGBA with an opaque alpha. Rows may carry padding at their end:
/// the stride is `data.len() / height`.
pub fn to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    format: PixelFormat,
) -> Result<Vec<u8>, CallError> {
    let bytes = match format.bits_per_pixel {
        24 => 3,
        32 => 4,
        n => {
            return Err(CallError::Failed(format!(
                "cannot read {n}-bit pixels; only 24 and 32 bits are supported"
            )));
        }
    };
    let channels = [
        channel(format.red_mask)?,
        channel(format.green_mask)?,
        channel(format.blue_mask)?,
    ];
    let (w, h) = (width as usize, height as usize);
    if h == 0 || w == 0 {
        return Err(CallError::Failed("the area is empty".into()));
    }
    let stride = data.len() / h;
    if stride < w * bytes {
        return Err(CallError::Failed(format!(
            "the image is too short: {} bytes for {width}x{height}",
            data.len()
        )));
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for row in data.chunks_exact(stride).take(h) {
        for px in row[..w * bytes].chunks_exact(bytes) {
            let value = px.iter().enumerate().fold(0u32, |acc, (i, &b)| {
                if format.big_endian {
                    (acc << 8) | u32::from(b)
                } else {
                    acc | (u32::from(b) << (8 * i))
                }
            });
            for (shift, bits) in channels {
                out.push(scale((value >> shift) & ((1u32 << bits) - 1), bits));
            }
            out.push(255);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screencapture_takes_the_main_screen_or_an_area() {
        let path = Path::new("/tmp/shot.png");
        assert_eq!(
            screencapture_args(None, path),
            ["-x", "-m", "/tmp/shot.png"]
        );
        let area = Rect {
            x: 10,
            y: -20,
            width: 300,
            height: 200,
        };
        assert_eq!(
            screencapture_args(Some(area), path),
            ["-x", "-R", "10,-20,300,200", "/tmp/shot.png"]
        );
    }

    #[test]
    fn empty_areas_are_refused() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 10,
        };
        assert_eq!(
            check_area(&area),
            Err(CallError::Failed("the area is empty".into()))
        );
        assert!(
            check_area(&Rect {
                width: 1,
                height: 1,
                ..area
            })
            .is_ok()
        );
    }

    #[test]
    fn grim_takes_everything_or_an_area() {
        let path = Path::new("/tmp/shot.png");
        assert_eq!(grim_args(None, path), ["/tmp/shot.png"]);
        let area = Rect {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
        };
        assert_eq!(
            grim_args(Some(area), path),
            ["-g", "10,20 300x200", "/tmp/shot.png"]
        );
    }

    #[test]
    fn areas_are_clipped_to_the_screen() {
        let r = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(
            clip_area(r(10, 20, 30, 40), 100, 100),
            Ok(r(10, 20, 30, 40))
        );
        assert_eq!(
            clip_area(r(-10, 90, 30, 40), 100, 100),
            Ok(r(0, 90, 20, 10))
        );
        assert_eq!(
            clip_area(r(0, 0, u32::MAX, u32::MAX), 1280, 800),
            Ok(r(0, 0, 1280, 800))
        );
        assert_eq!(
            clip_area(r(100, 0, 10, 10), 100, 100),
            Err(CallError::Failed("the area is outside the screen".into()))
        );
        assert!(clip_area(r(-20, 0, 10, 10), 100, 100).is_err());
    }

    const BGRX: PixelFormat = PixelFormat {
        bits_per_pixel: 32,
        red_mask: 0xff0000,
        green_mask: 0x00ff00,
        blue_mask: 0x0000ff,
        big_endian: false,
    };

    #[test]
    fn little_endian_32_bit_pixels_become_rgba() {
        // Two pixels: pure red, then 0x112233; the fourth byte is padding.
        let data = [0, 0, 0xff, 0x99, 0x33, 0x22, 0x11, 0x00];
        assert_eq!(
            to_rgba(&data, 2, 1, BGRX).unwrap(),
            [0xff, 0, 0, 255, 0x11, 0x22, 0x33, 255]
        );
    }

    #[test]
    fn big_endian_pixels_follow_the_byte_order() {
        let format = PixelFormat {
            big_endian: true,
            ..BGRX
        };
        let data = [0x00, 0x11, 0x22, 0x33];
        assert_eq!(
            to_rgba(&data, 1, 1, format).unwrap(),
            [0x11, 0x22, 0x33, 255]
        );
        // 24-bit pixels, rows padded to 4 bytes.
        let format = PixelFormat {
            bits_per_pixel: 24,
            ..format
        };
        let data = [0x11, 0x22, 0x33, 0, 0x44, 0x55, 0x66, 0];
        assert_eq!(
            to_rgba(&data, 1, 2, format).unwrap(),
            [0x11, 0x22, 0x33, 255, 0x44, 0x55, 0x66, 255]
        );
    }

    #[test]
    fn other_masks_and_depths() {
        // RGB565 in 32-bit pixels (as a test of the masks, not a real visual).
        let format = PixelFormat {
            red_mask: 0xf800,
            green_mask: 0x07e0,
            blue_mask: 0x001f,
            ..BGRX
        };
        let data = 0xf800u32.to_le_bytes();
        assert_eq!(to_rgba(&data, 1, 1, format).unwrap(), [255, 0, 0, 255]);
        // 10 bits a channel keep the top 8.
        let format = PixelFormat {
            red_mask: 0x3ff << 20,
            green_mask: 0x3ff << 10,
            blue_mask: 0x3ff,
            ..BGRX
        };
        let data = ((0x3ffu32 << 20) | (0x200 << 10)).to_le_bytes();
        assert_eq!(to_rgba(&data, 1, 1, format).unwrap(), [255, 128, 0, 255]);

        let bad = PixelFormat {
            bits_per_pixel: 16,
            ..BGRX
        };
        assert!(matches!(
            to_rgba(&[0; 4], 2, 1, bad),
            Err(CallError::Failed(_))
        ));
        assert!(matches!(
            to_rgba(&[0; 4], 2, 1, BGRX),
            Err(CallError::Failed(_))
        ));
    }

    /// Needs a GUI session and the Screen Recording permission for the test
    /// binary's terminal.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn macos_screencapture_writes_a_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        let status = std::process::Command::new("/usr/sbin/screencapture")
            .args(screencapture_args(None, &path))
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"\x89PNG");
    }
}
