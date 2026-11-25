//! `screen` on Windows: a GDI copy of the screen (`BitBlt` from the
//! screen's DC with `CAPTUREBLT`, read back with `GetDIBits`), encoded as
//! PNG here. Without an area it takes the whole virtual screen, every
//! monitor; an area is in the coordinates `window.screens` reports
//! (physical pixels, the primary monitor's corner at 0,0) and is clipped to
//! the virtual screen. Windows asks for no permission. On the secure
//! desktop (UAC, the lock screen) the copy fails and the call answers
//! `failed`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HBITMAP, HDC,
    HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::wm;
use crate::caller::Caller;
use crate::error::CallError;
use crate::screen::{PixelFormat, ScreenProvider, check_area, clip_area, to_rgba};
use crate::sensors::{Sensor, Sensors};
use crate::window::Rect;

/// A 32-bit DIB: blue, green, red, then a padding byte, little-endian.
const BGRX: PixelFormat = PixelFormat {
    bits_per_pixel: 32,
    red_mask: 0x00ff_0000,
    green_mask: 0x0000_ff00,
    blue_mask: 0x0000_00ff,
    big_endian: false,
};

/// Writes `<data_dir>/<plugin>/screenshots/<ms>.png`, as on macOS.
pub struct GdiScreen {
    data_dir: PathBuf,
    sensors: Arc<Sensors>,
}

impl GdiScreen {
    pub fn new(data_dir: PathBuf, sensors: Arc<Sensors>) -> Self {
        wm::ensure_dpi_aware();
        Self { data_dir, sensors }
    }

    async fn shot_path(&self, plugin: &str) -> Result<PathBuf, CallError> {
        let dir = self.data_dir.join(plugin).join("screenshots");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| CallError::Failed(format!("cannot create {}: {e}", dir.display())))?;
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        Ok(dir.join(format!("{ms}.png")))
    }
}

#[async_trait]
impl ScreenProvider for GdiScreen {
    async fn capture(&self, caller: &Caller, area: Option<Rect>) -> Result<String, CallError> {
        if let Some(a) = &area {
            check_area(a)?;
        }
        let path = self.shot_path(&caller.plugin).await?;
        self.sensors.set(&caller.plugin, Sensor::Screen, true);
        let to = path.clone();
        let result = tokio::task::spawn_blocking(move || capture(area, &to))
            .await
            .map_err(CallError::failed)
            .and_then(|r| r);
        self.sensors.set(&caller.plugin, Sensor::Screen, false);
        match result {
            Ok(()) => Ok(path.display().to_string()),
            Err(e) => {
                let _ = tokio::fs::remove_file(&path).await;
                Err(e)
            }
        }
    }
}

/// The screen DC, a memory DC and a bitmap, released in reverse order.
struct Gdi {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}

impl Drop for Gdi {
    fn drop(&mut self) {
        // SAFETY: the objects `capture` made, each released once.
        unsafe {
            if !self.old.is_invalid() {
                SelectObject(self.memory, self.old);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.memory.is_invalid() {
                let _ = DeleteDC(self.memory);
            }
            ReleaseDC(None, self.screen);
        }
    }
}

fn cannot(what: &str) -> CallError {
    CallError::Failed(format!("cannot {what} the screen"))
}

/// Copies the virtual screen, or `area` of it, and writes the PNG.
fn capture(area: Option<Rect>, path: &Path) -> Result<(), CallError> {
    // SAFETY: plain reads.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let (vw, vh) = (
        u32::try_from(vw).map_err(|_| cannot("measure"))?,
        u32::try_from(vh).map_err(|_| cannot("measure"))?,
    );
    // Clip in the virtual screen's own coordinates, its corner at 0,0.
    let local = match area {
        Some(a) => clip_area(
            Rect {
                x: a.x.saturating_sub(vx),
                y: a.y.saturating_sub(vy),
                ..a
            },
            vw,
            vh,
        )?,
        None => Rect {
            x: 0,
            y: 0,
            width: vw,
            height: vh,
        },
    };
    if local.width == 0 || local.height == 0 {
        return Err(CallError::Failed("the area is empty".into()));
    }
    // Both fit in i32: they lie on the virtual screen.
    let (w, h) = (local.width as i32, local.height as i32);
    // SAFETY: every GDI object is checked and released by `Gdi`'s drop;
    // the pixel buffer holds `w * h` 32-bit pixels, as the header says.
    let pixels = unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err(cannot("open"));
        }
        let mut gdi = Gdi {
            screen,
            memory: HDC::default(),
            bitmap: HBITMAP::default(),
            old: HGDIOBJ::default(),
        };
        gdi.memory = CreateCompatibleDC(Some(screen));
        gdi.bitmap = CreateCompatibleBitmap(screen, w, h);
        if gdi.memory.is_invalid() || gdi.bitmap.is_invalid() {
            return Err(cannot("copy"));
        }
        gdi.old = SelectObject(gdi.memory, gdi.bitmap.into());
        BitBlt(
            gdi.memory,
            0,
            0,
            w,
            h,
            Some(screen),
            local.x + vx,
            local.y + vy,
            SRCCOPY | CAPTUREBLT,
        )
        .map_err(|e| CallError::Failed(format!("cannot copy the screen: {e}")))?;
        // GetDIBits wants the bitmap out of any DC.
        SelectObject(gdi.memory, gdi.old);
        gdi.old = HGDIOBJ::default();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // Negative: rows top to bottom.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; local.width as usize * local.height as usize * 4];
        let rows = GetDIBits(
            gdi.memory,
            gdi.bitmap,
            0,
            local.height,
            Some(pixels.as_mut_ptr().cast()),
            &raw mut info,
            DIB_RGB_COLORS,
        );
        if rows != h {
            return Err(cannot("read"));
        }
        pixels
    };
    let rgba = to_rgba(&pixels, local.width, local.height, BGRX)?;
    write_png(path, local.width, local.height, &rgba)
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), CallError> {
    let cannot = |e: &dyn std::fmt::Display| {
        CallError::Failed(format!("cannot write {}: {e}", path.display()))
    };
    let file = std::fs::File::create(path).map_err(|e| cannot(&e))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| cannot(&e))?;
    writer.write_image_data(rgba).map_err(|e| cannot(&e))?;
    writer.finish().map_err(|e| cannot(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_pixels_become_rgba() {
        // Blue, green, red, padding.
        let data = [0x33, 0x22, 0x11, 0x00, 0x00, 0x00, 0xff, 0x00];
        assert_eq!(
            to_rgba(&data, 2, 1, BGRX).unwrap(),
            [0x11, 0x22, 0x33, 255, 0xff, 0, 0, 255]
        );
    }

    #[test]
    fn png_files_have_the_size_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        write_png(&path, 3, 2, &[7; 3 * 2 * 4]).unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (3, 2));
    }
}
