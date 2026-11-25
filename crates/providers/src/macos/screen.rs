//! `screen` through `/usr/sbin/screencapture`. Without the Screen
//! Recording permission it writes nothing or an empty file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;
use crate::screen::{ScreenProvider, check_area, screencapture_args};
use crate::sensors::{Sensor, Sensors};
use crate::window::Rect;

const PROGRAM: &str = "/usr/sbin/screencapture";

/// Writes `<data_dir>/<plugin>/screenshots/<ms>.png`.
pub struct Screencapture {
    data_dir: PathBuf,
    sensors: Arc<Sensors>,
}

impl Screencapture {
    pub fn new(data_dir: PathBuf, sensors: Arc<Sensors>) -> Self {
        Self { data_dir, sensors }
    }
}

#[async_trait]
impl ScreenProvider for Screencapture {
    async fn capture(&self, caller: &Caller, area: Option<Rect>) -> Result<String, CallError> {
        if let Some(a) = &area {
            check_area(a)?;
        }
        let dir = self.data_dir.join(&caller.plugin).join("screenshots");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| CallError::Failed(format!("cannot create {}: {e}", dir.display())))?;
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let path = dir.join(format!("{ms}.png"));
        self.sensors.set(&caller.plugin, Sensor::Screen, true);
        let out = tokio::process::Command::new(PROGRAM)
            .args(screencapture_args(area, &path))
            .output()
            .await;
        self.sensors.set(&caller.plugin, Sensor::Screen, false);
        let out = out.map_err(|e| CallError::Failed(format!("cannot run {PROGRAM}: {e}")))?;
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let size = tokio::fs::metadata(&path).await.map_or(0, |m| m.len());
        if size > 0 && out.status.success() {
            return Ok(path.display().to_string());
        }
        let _ = tokio::fs::remove_file(&path).await;
        if out.status.success() {
            // Exit 0 with no picture: the permission is missing.
            return Err(CallError::Denied);
        }
        Err(CallError::Failed(if stderr.is_empty() {
            format!("screencapture exited with {}", out.status)
        } else {
            stderr
        }))
    }
}
