//! The DirectShow microphones and cameras that `mic` and `camera` record
//! from. DirectShow has no "default" device, so without a `device` in
//! node.toml the node asks ffmpeg for the list once and keeps it.

use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::mic::{DshowDevices, parse_dshow_devices};

/// `CREATE_NO_WINDOW`: no console flashes up for ffmpeg.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The devices `ffmpeg -list_devices true -f dshow` printed the first time
/// this ran; none when ffmpeg is not on `PATH`.
pub fn listed() -> DshowDevices {
    static DEVICES: OnceLock<DshowDevices> = OnceLock::new();
    DEVICES.get_or_init(list).clone()
}

fn list() -> DshowDevices {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-list_devices",
            "true",
            "-f",
            "dshow",
            "-i",
            "dummy",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(out) => {
            let devices = parse_dshow_devices(&String::from_utf8_lossy(&out.stderr));
            tracing::debug!(?devices, "DirectShow devices");
            devices
        }
        Err(e) => {
            tracing::debug!("cannot list DirectShow devices with ffmpeg: {e}");
            DshowDevices::default()
        }
    }
}
