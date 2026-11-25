//! `screen` on Linux. On X11 it reads the root window with `GetImage` on a
//! connection opened for each call, converts the pixels of a 24- or 32-bit
//! TrueColor visual to RGBA and encodes the PNG itself. An area is in
//! root-window pixels, as `window.screens` reports them, and is clipped to
//! the root window. On Wayland it runs `grim` when it is on `PATH`, else it
//! answers `unavailable`; without a display it answers `unavailable`. Both
//! log why the first time.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder, VisualClass};

use super::{Session, warn_once, which};
use crate::caller::Caller;
use crate::error::CallError;
use crate::screen::{PixelFormat, ScreenProvider, check_area, clip_area, grim_args, to_rgba};
use crate::sensors::{Sensor, Sensors};
use crate::window::Rect;

/// Writes `<data_dir>/<plugin>/screenshots/<ms>.png`, as on macOS.
pub struct LinuxScreen {
    data_dir: PathBuf,
    sensors: Arc<Sensors>,
    warned: AtomicBool,
}

impl LinuxScreen {
    pub fn new(data_dir: PathBuf, sensors: Arc<Sensors>) -> Self {
        Self {
            data_dir,
            sensors,
            warned: AtomicBool::new(false),
        }
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
impl ScreenProvider for LinuxScreen {
    async fn capture(&self, caller: &Caller, area: Option<Rect>) -> Result<String, CallError> {
        if let Some(a) = &area {
            check_area(a)?;
        }
        let grim = match Session::current() {
            Session::X11 => None,
            Session::Wayland => match which("grim") {
                Some(grim) => Some(grim),
                None => {
                    warn_once(
                        &self.warned,
                        "screen: this is a Wayland session and grim is not on PATH, so screen \
                         answers unavailable (see docs/linux.md)",
                    );
                    return Err(CallError::Unavailable);
                }
            },
            Session::Headless => {
                warn_once(
                    &self.warned,
                    "screen: no DISPLAY, so screen answers unavailable",
                );
                return Err(CallError::Unavailable);
            }
        };
        let path = self.shot_path(&caller.plugin).await?;
        self.sensors.set(&caller.plugin, Sensor::Screen, true);
        let result = match grim {
            Some(grim) => run_grim(&grim, area, &path).await,
            None => {
                let to = path.clone();
                tokio::task::spawn_blocking(move || x11_capture(area, &to))
                    .await
                    .map_err(CallError::failed)
                    .and_then(|r| r)
            }
        };
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

async fn run_grim(grim: &Path, area: Option<Rect>, path: &Path) -> Result<(), CallError> {
    let out = tokio::process::Command::new(grim)
        .args(grim_args(area, path))
        .output()
        .await
        .map_err(|e| CallError::Failed(format!("cannot run grim: {e}")))?;
    let size = tokio::fs::metadata(path).await.map_or(0, |m| m.len());
    if out.status.success() && size > 0 {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(CallError::Failed(if stderr.is_empty() {
        format!("grim exited with {}", out.status)
    } else {
        format!("grim: {stderr}")
    }))
}

fn failed(e: impl std::fmt::Display) -> CallError {
    CallError::Failed(format!("X11: {e}"))
}

/// Reads the root window, or `area` of it, and writes the PNG to `path`.
fn x11_capture(area: Option<Rect>, path: &Path) -> Result<(), CallError> {
    x11_capture_on(None, area, path)
}

/// [`x11_capture`] on `display` (`:99`), or the default display.
pub(super) fn x11_capture_on(
    display: Option<&str>,
    area: Option<Rect>,
    path: &Path,
) -> Result<(), CallError> {
    let (conn, screen) = x11rb::connect(display).map_err(|e| {
        tracing::debug!("cannot open the X display: {e}");
        CallError::Unavailable
    })?;
    let setup = conn.setup();
    let root = &setup.roots[screen];
    let (width, height) = (
        u32::from(root.width_in_pixels),
        u32::from(root.height_in_pixels),
    );
    let area = match area {
        Some(a) => clip_area(a, width, height)?,
        None => Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
    };
    // The clipped area lies on the root window, whose size fits in u16.
    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            root.root,
            area.x as i16,
            area.y as i16,
            area.width as u16,
            area.height as u16,
            !0,
        )
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    let visual = root
        .allowed_depths
        .iter()
        .flat_map(|d| &d.visuals)
        .find(|v| v.visual_id == image.visual)
        .ok_or_else(|| failed("the root window's visual is not listed"))?;
    if visual.class != VisualClass::TRUE_COLOR && visual.class != VisualClass::DIRECT_COLOR {
        return Err(CallError::Failed(format!(
            "cannot read a {:?} screen; only TrueColor is supported",
            visual.class
        )));
    }
    let bits_per_pixel = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == image.depth)
        .map(|f| f.bits_per_pixel)
        .ok_or_else(|| failed(format!("no pixmap format for depth {}", image.depth)))?;
    let rgba = to_rgba(
        &image.data,
        area.width,
        area.height,
        PixelFormat {
            bits_per_pixel,
            red_mask: visual.red_mask,
            green_mask: visual.green_mask,
            blue_mask: visual.blue_mask,
            big_endian: setup.image_byte_order == ImageOrder::MSB_FIRST,
        },
    )?;
    write_png(path, area.width, area.height, &rgba)
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
    fn png_files_have_the_size_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        write_png(&path, 3, 2, &[7; 3 * 2 * 4]).unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (3, 2));
        assert_eq!(reader.info().color_type, png::ColorType::Rgba);
    }
}
