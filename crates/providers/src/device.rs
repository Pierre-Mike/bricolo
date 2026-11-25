//! `device`: volume, brightness, Wi-Fi, dark mode, Focus, lock and sleep.
//! Most of it runs the system's own programs, through a [`Runner`]: the
//! plans for each system live here and the tests check them with a fake
//! runner on any system. What has no program goes through [`Native`]:
//! brightness and locking on macOS (`macos/device.rs`).
//!
//! | | macOS | Linux | Windows |
//! |---|---|---|---|
//! | volume, mute | `osascript` | `wpctl`, else `pactl` | PowerShell, Core Audio |
//! | brightness | DisplayServices | `brightnessctl` | PowerShell, WMI |
//! | Wi-Fi | `networksetup` | `nmcli` | PowerShell, WinRT radios |
//! | dark mode | `defaults`, System Events | `gsettings` | `reg` |
//! | Focus | Shortcuts, `Assertions.json` | GNOME banners | unavailable |
//! | lock | `SACLockScreenImmediate` | `loginctl` | `LockWorkStation` |
//! | sleep | `pmset` | `systemctl` | `SetSuspendState` |
//!
//! `device` never runs on a peer through rules.toml. The MCP gateway marks
//! `lock`, `sleep` and `set-wifi` as destructive: each can cut the agent
//! off from this node.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::caller::Caller;
use crate::cmd::{RunError, Runner};
use crate::error::CallError;

const TIMEOUT: Duration = Duration::from_secs(20);

#[async_trait]
pub trait DeviceProvider: Send + Sync {
    async fn volume(&self, caller: &Caller) -> Result<u8, CallError>;
    async fn set_volume(&self, caller: &Caller, level: u8) -> Result<(), CallError>;
    async fn muted(&self, caller: &Caller) -> Result<bool, CallError>;
    async fn set_muted(&self, caller: &Caller, muted: bool) -> Result<(), CallError>;
    async fn brightness(&self, caller: &Caller) -> Result<u8, CallError>;
    async fn set_brightness(&self, caller: &Caller, level: u8) -> Result<(), CallError>;
    async fn wifi(&self, caller: &Caller) -> Result<bool, CallError>;
    async fn set_wifi(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    async fn dark_mode(&self, caller: &Caller) -> Result<bool, CallError>;
    async fn set_dark_mode(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    async fn focus_mode(&self, caller: &Caller) -> Result<bool, CallError>;
    async fn set_focus_mode(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    async fn lock(&self, caller: &Caller) -> Result<(), CallError>;
    async fn sleep(&self, caller: &Caller) -> Result<(), CallError>;
}

/// No device controls on this node.
pub struct NoDevice;

#[async_trait]
impl DeviceProvider for NoDevice {
    async fn volume(&self, _: &Caller) -> Result<u8, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_volume(&self, _: &Caller, _: u8) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn muted(&self, _: &Caller) -> Result<bool, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_muted(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn brightness(&self, _: &Caller) -> Result<u8, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_brightness(&self, _: &Caller, _: u8) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn wifi(&self, _: &Caller) -> Result<bool, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_wifi(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn dark_mode(&self, _: &Caller) -> Result<bool, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_dark_mode(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn focus_mode(&self, _: &Caller) -> Result<bool, CallError> {
        Err(CallError::Unavailable)
    }
    async fn set_focus_mode(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn lock(&self, _: &Caller) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn sleep(&self, _: &Caller) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

/// Which system's plans to follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    pub fn here() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Os::MacOs)
        } else if cfg!(target_os = "linux") {
            Some(Os::Linux)
        } else if cfg!(target_os = "windows") {
            Some(Os::Windows)
        } else {
            None
        }
    }
}

/// What a system does without a program. `None` means "use the plan".
pub trait Native: Send + Sync + 'static {
    fn brightness(&self) -> Option<Result<u8, CallError>> {
        None
    }
    fn set_brightness(&self, _level: u8) -> Option<Result<(), CallError>> {
        None
    }
    fn lock(&self) -> Option<Result<(), CallError>> {
        None
    }
}

pub struct NoNative;
impl Native for NoNative {}

/// The device controls of one system.
pub struct Device<R: Runner> {
    inner: Arc<Plans<R>>,
}

struct Plans<R: Runner> {
    os: Os,
    runner: R,
    native: Box<dyn Native>,
    home: PathBuf,
}

impl<R: Runner> Device<R> {
    pub fn new(os: Os, runner: R, native: Box<dyn Native>, home: PathBuf) -> Self {
        Self {
            inner: Arc::new(Plans {
                os,
                runner,
                native,
                home,
            }),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Plans<R>) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let p = self.inner.clone();
        tokio::task::spawn_blocking(move || f(&p))
            .await
            .map_err(CallError::failed)?
    }
}

fn level(level: u8) -> Result<u8, CallError> {
    if level > 100 {
        return Err(CallError::Failed(format!(
            "invalid_argument: level {level} is above 100"
        )));
    }
    Ok(level)
}

fn unparsable(what: &str, text: &str) -> CallError {
    CallError::Failed(format!("cannot read {what} from {text:?}"))
}

/// `powershell.exe` with the script as `-EncodedCommand`, so here-strings
/// and quotes pass untouched.
pub fn powershell_args(script: &str) -> Vec<String> {
    let utf16: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-EncodedCommand".into(),
        base64(&utf16),
    ]
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Core Audio's endpoint volume, declared for PowerShell. The interface
/// slots before each method used are placeholders, in vtable order.
const WIN_AUDIO: &str = r#"Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IRhEndpointVolume {
  int A(); int B(); int C(); int D();
  int SetMasterVolumeLevelScalar(float level, ref Guid context);
  int E();
  int GetMasterVolumeLevelScalar(out float level);
  int F(); int G(); int H(); int I();
  int SetMute([MarshalAs(UnmanagedType.Bool)] bool mute, ref Guid context);
  int GetMute([MarshalAs(UnmanagedType.Bool)] out bool mute);
}
[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IRhDevice { int Activate(ref Guid iid, int ctx, IntPtr p, [MarshalAs(UnmanagedType.IUnknown)] out object o); }
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IRhEnumerator { int A(); int GetDefaultAudioEndpoint(int flow, int role, out IRhDevice d); }
[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class RhEnumerator { }
public static class RhAudio {
  static IRhEndpointVolume V() {
    var e = (IRhEnumerator)new RhEnumerator(); IRhDevice d;
    Marshal.ThrowExceptionForHR(e.GetDefaultAudioEndpoint(0, 1, out d));
    var iid = typeof(IRhEndpointVolume).GUID; object o;
    Marshal.ThrowExceptionForHR(d.Activate(ref iid, 23, IntPtr.Zero, out o));
    return (IRhEndpointVolume)o;
  }
  public static float GetVolume() { float v; Marshal.ThrowExceptionForHR(V().GetMasterVolumeLevelScalar(out v)); return v; }
  public static void SetVolume(float v) { var g = Guid.Empty; Marshal.ThrowExceptionForHR(V().SetMasterVolumeLevelScalar(v, ref g)); }
  public static bool GetMute() { bool m; Marshal.ThrowExceptionForHR(V().GetMute(out m)); return m; }
  public static void SetMute(bool m) { var g = Guid.Empty; Marshal.ThrowExceptionForHR(V().SetMute(m, ref g)); }
}
'@
"#;

/// The Wi-Fi radio through WinRT, as `$wifi`.
const WIN_RADIO: &str = r#"Add-Type -AssemblyName System.Runtime.WindowsRuntime
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
function Await($op, $type) { $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op)); $t.Wait(-1) | Out-Null; $t.Result }
[Windows.Devices.Radios.Radio, Windows.System.Devices, ContentType = WindowsRuntime] | Out-Null
Await ([Windows.Devices.Radios.Radio]::RequestAccessAsync()) ([Windows.Devices.Radios.RadioAccessStatus]) | Out-Null
$radios = Await ([Windows.Devices.Radios.Radio]::GetRadiosAsync()) ([System.Collections.Generic.IReadOnlyList[Windows.Devices.Radios.Radio]])
$wifi = $radios | Where-Object { $_.Kind -eq 'WiFi' } | Select-Object -First 1
if (-not $wifi) { [Console]::Error.WriteLine('no Wi-Fi radio'); exit 2 }
"#;

const WIN_THEME: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// `wpctl get-volume`: "Volume: 0.40 [MUTED]".
pub fn parse_wpctl(text: &str) -> Option<(u8, bool)> {
    let v: f64 = text
        .trim()
        .strip_prefix("Volume:")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some((
        (v * 100.0).round().clamp(0.0, 100.0) as u8,
        text.contains("[MUTED]"),
    ))
}

/// `pactl get-sink-volume`: the first channel's "40%".
pub fn parse_percent(text: &str) -> Option<u8> {
    let at = text.find('%')?;
    let digits: String = text[..at]
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse::<u32>().ok().map(|v| v.min(100) as u8)
}

/// `networksetup -listallhardwareports`: the device of the Wi-Fi port.
pub fn wifi_device(text: &str) -> Option<String> {
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "Hardware Port: Wi-Fi" || l.trim() == "Hardware Port: AirPort" {
            return lines
                .next()?
                .trim()
                .strip_prefix("Device:")
                .map(|d| d.trim().to_string());
        }
    }
    None
}

impl<R: Runner> Plans<R> {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, CallError> {
        self.runner
            .run(program, args, None, TIMEOUT)
            .map_err(CallError::from)?
            .text(program)
    }

    /// The first of `programs` that is installed.
    fn first(&self, plans: &[(&str, &[&str])]) -> Result<(String, &'static str), CallError> {
        for (program, args) in plans {
            match self.runner.run(program, args, None, TIMEOUT) {
                Err(RunError::Missing) => continue,
                Err(e) => return Err(e.into()),
                Ok(r) => {
                    return r.text(program).map(|t| {
                        // Which plan answered, as a static name.
                        let which = if program.ends_with("wpctl") {
                            "wpctl"
                        } else if program.ends_with("pactl") {
                            "pactl"
                        } else {
                            "other"
                        };
                        (t, which)
                    });
                }
            }
        }
        Err(CallError::Failed(format!(
            "none of {} is installed",
            plans.iter().map(|(p, _)| *p).collect::<Vec<_>>().join(", ")
        )))
    }

    fn ps(&self, script: &str) -> Result<String, CallError> {
        let args = powershell_args(script);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run("powershell.exe", &args)
    }

    fn osascript(&self, line: &str) -> Result<String, CallError> {
        self.run("/usr/bin/osascript", &["-e", line])
    }

    fn mac_wifi_device(&self) -> Result<String, CallError> {
        let ports = self.run("/usr/sbin/networksetup", &["-listallhardwareports"])?;
        wifi_device(&ports).ok_or_else(|| CallError::Failed("this Mac has no Wi-Fi port".into()))
    }

    fn volume(&self) -> Result<u8, CallError> {
        match self.os {
            Os::MacOs => {
                let t = self.osascript("output volume of (get volume settings)")?;
                t.parse().map_err(|_| {
                    if t == "missing value" {
                        CallError::Failed("the output device has no volume control".into())
                    } else {
                        unparsable("the volume", &t)
                    }
                })
            }
            Os::Linux => {
                let (t, which) = self.first(&[
                    ("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]),
                    ("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]),
                ])?;
                match which {
                    "wpctl" => parse_wpctl(&t).map(|(v, _)| v),
                    _ => parse_percent(&t),
                }
                .ok_or_else(|| unparsable("the volume", &t))
            }
            Os::Windows => {
                let t = self.ps(&format!(
                    "{WIN_AUDIO}[math]::Round([RhAudio]::GetVolume() * 100)"
                ))?;
                t.parse().map_err(|_| unparsable("the volume", &t))
            }
        }
    }

    fn set_volume(&self, level: u8) -> Result<(), CallError> {
        match self.os {
            Os::MacOs => self
                .osascript(&format!("set volume output volume {level}"))
                .map(drop),
            Os::Linux => {
                let wp = format!("{:.2}", f64::from(level) / 100.0);
                let pa = format!("{level}%");
                self.first(&[
                    ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &wp]),
                    ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &pa]),
                ])
                .map(drop)
            }
            Os::Windows => self
                .ps(&format!("{WIN_AUDIO}[RhAudio]::SetVolume({level} / 100.0)"))
                .map(drop),
        }
    }

    fn muted(&self) -> Result<bool, CallError> {
        match self.os {
            Os::MacOs => Ok(self.osascript("output muted of (get volume settings)")? == "true"),
            Os::Linux => {
                let (t, which) = self.first(&[
                    ("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]),
                    ("pactl", &["get-sink-mute", "@DEFAULT_SINK@"]),
                ])?;
                match which {
                    "wpctl" => parse_wpctl(&t)
                        .map(|(_, m)| m)
                        .ok_or_else(|| unparsable("mute", &t)),
                    _ => Ok(t.contains("yes")),
                }
            }
            Os::Windows => Ok(self.ps(&format!("{WIN_AUDIO}[RhAudio]::GetMute()"))? == "True"),
        }
    }

    fn set_muted(&self, muted: bool) -> Result<(), CallError> {
        let n = if muted { "1" } else { "0" };
        match self.os {
            Os::MacOs => self
                .osascript(&format!("set volume output muted {muted}"))
                .map(drop),
            Os::Linux => self
                .first(&[
                    ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", n]),
                    ("pactl", &["set-sink-mute", "@DEFAULT_SINK@", n]),
                ])
                .map(drop),
            Os::Windows => self
                .ps(&format!(
                    "{WIN_AUDIO}[RhAudio]::SetMute(${})",
                    if muted { "true" } else { "false" }
                ))
                .map(drop),
        }
    }

    fn brightness(&self) -> Result<u8, CallError> {
        if let Some(r) = self.native.brightness() {
            return r;
        }
        match self.os {
            Os::MacOs => Err(CallError::Unavailable),
            Os::Linux => {
                // "intel_backlight,backlight,240,25%,960"
                let t = self.run("brightnessctl", &["-m"])?;
                t.split(',')
                    .nth(3)
                    .and_then(parse_percent)
                    .ok_or_else(|| unparsable("the brightness", &t))
            }
            Os::Windows => {
                let t = self.ps(
                    "(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness | Select-Object -First 1).CurrentBrightness",
                )?;
                t.parse().map_err(|_| {
                    if t.is_empty() {
                        CallError::Failed("no display here reports its brightness".into())
                    } else {
                        unparsable("the brightness", &t)
                    }
                })
            }
        }
    }

    fn set_brightness(&self, level: u8) -> Result<(), CallError> {
        if let Some(r) = self.native.set_brightness(level) {
            return r;
        }
        match self.os {
            Os::MacOs => Err(CallError::Unavailable),
            Os::Linux => self.run("brightnessctl", &["set", &format!("{level}%")]).map(drop),
            Os::Windows => self
                .ps(&format!(
                    "Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods | Invoke-CimMethod -MethodName WmiSetBrightness -Arguments @{{Timeout=0; Brightness={level}}} | Out-Null"
                ))
                .map(drop),
        }
    }

    fn wifi(&self) -> Result<bool, CallError> {
        match self.os {
            Os::MacOs => {
                let dev = self.mac_wifi_device()?;
                let t = self.run("/usr/sbin/networksetup", &["-getairportpower", &dev])?;
                Ok(t.trim_end().ends_with("On"))
            }
            Os::Linux => Ok(self.run("nmcli", &["radio", "wifi"])? == "enabled"),
            Os::Windows => Ok(self.ps(&format!("{WIN_RADIO}$wifi.State.ToString()"))? == "On"),
        }
    }

    fn set_wifi(&self, on: bool) -> Result<(), CallError> {
        let word = if on { "on" } else { "off" };
        match self.os {
            Os::MacOs => {
                let dev = self.mac_wifi_device()?;
                self.run("/usr/sbin/networksetup", &["-setairportpower", &dev, word])
                    .map(drop)
            }
            Os::Linux => self.run("nmcli", &["radio", "wifi", word]).map(drop),
            Os::Windows => self
                .ps(&format!(
                    "{WIN_RADIO}Await ($wifi.SetStateAsync('{}')) ([Windows.Devices.Radios.RadioAccessStatus]) | Out-Null",
                    if on { "On" } else { "Off" }
                ))
                .map(drop),
        }
    }

    fn dark_mode(&self) -> Result<bool, CallError> {
        match self.os {
            // Exits 1 when the key is missing, which means light.
            Os::MacOs => match self.runner.run(
                "/usr/bin/defaults",
                &["read", "-g", "AppleInterfaceStyle"],
                None,
                TIMEOUT,
            ) {
                Ok(r) => Ok(r.ok() && r.stdout.trim() == "Dark"),
                Err(e) => Err(e.into()),
            },
            Os::Linux => Ok(self
                .run(
                    "gsettings",
                    &["get", "org.gnome.desktop.interface", "color-scheme"],
                )?
                .contains("dark")),
            Os::Windows => {
                let t = self.run("reg", &["query", WIN_THEME, "/v", "AppsUseLightTheme"])?;
                Ok(t.contains("0x0"))
            }
        }
    }

    fn set_dark_mode(&self, on: bool) -> Result<(), CallError> {
        match self.os {
            Os::MacOs => self
                .osascript(&format!(
                    "tell application \"System Events\" to tell appearance preferences to set dark mode to {on}"
                ))
                .map(drop),
            Os::Linux => self
                .run(
                    "gsettings",
                    &[
                        "set",
                        "org.gnome.desktop.interface",
                        "color-scheme",
                        if on { "prefer-dark" } else { "default" },
                    ],
                )
                .map(drop),
            Os::Windows => {
                let light = if on { "0" } else { "1" };
                for key in ["AppsUseLightTheme", "SystemUsesLightTheme"] {
                    self.run(
                        "reg",
                        &["add", WIN_THEME, "/v", key, "/t", "REG_DWORD", "/d", light, "/f"],
                    )?;
                }
                Ok(())
            }
        }
    }

    fn focus_mode(&self) -> Result<bool, CallError> {
        match self.os {
            Os::MacOs => {
                let path = self.home.join("Library/DoNotDisturb/DB/Assertions.json");
                let text = std::fs::read_to_string(&path).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::PermissionDenied {
                        CallError::Failed(
                            "reading Focus needs Full Disk Access for the node, in System Settings > Privacy & Security".into(),
                        )
                    } else {
                        CallError::Failed(format!("cannot read {}: {e}", path.display()))
                    }
                })?;
                Ok(mac_focus_on(&text))
            }
            Os::Linux => Ok(self.run(
                "gsettings",
                &["get", "org.gnome.desktop.notifications", "show-banners"],
            )? == "false"),
            Os::Windows => Err(CallError::Unavailable),
        }
    }

    fn set_focus_mode(&self, on: bool) -> Result<(), CallError> {
        match self.os {
            Os::MacOs => {
                let name = if on {
                    "bricolo focus on"
                } else {
                    "bricolo focus off"
                };
                let list = self.run("/usr/bin/shortcuts", &["list"])?;
                if !list.lines().any(|l| l.trim() == name) {
                    return Err(CallError::Failed(format!(
                        "macOS lets only Shortcuts change Focus: make a shortcut named {name:?} with \
                         the action Set Focus (Do Not Disturb, turned {}), then try again",
                        if on { "on" } else { "off" }
                    )));
                }
                self.run("/usr/bin/shortcuts", &["run", name]).map(drop)
            }
            Os::Linux => self
                .run(
                    "gsettings",
                    &[
                        "set",
                        "org.gnome.desktop.notifications",
                        "show-banners",
                        if on { "false" } else { "true" },
                    ],
                )
                .map(drop),
            Os::Windows => Err(CallError::Unavailable),
        }
    }

    fn lock(&self) -> Result<(), CallError> {
        if let Some(r) = self.native.lock() {
            return r;
        }
        match self.os {
            Os::MacOs => Err(CallError::Unavailable),
            Os::Linux => self.run("loginctl", &["lock-session"]).map(drop),
            Os::Windows => self
                .run("rundll32.exe", &["user32.dll,LockWorkStation"])
                .map(drop),
        }
    }

    fn sleep(&self) -> Result<(), CallError> {
        match self.os {
            Os::MacOs => self.run("/usr/bin/pmset", &["sleepnow"]).map(drop),
            Os::Linux => self.run("systemctl", &["suspend"]).map(drop),
            Os::Windows => self
                .ps(
                    "Add-Type -AssemblyName System.Windows.Forms; [void][System.Windows.Forms.Application]::SetSuspendState('Suspend', $false, $false)",
                )
                .map(drop),
        }
    }
}

/// Focus is on while `Assertions.json` holds an assertion record.
pub fn mac_focus_on(text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    v["data"].as_array().into_iter().flatten().any(|d| {
        d["storeAssertionRecords"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    })
}

#[async_trait]
impl<R: Runner> DeviceProvider for Device<R> {
    async fn volume(&self, _: &Caller) -> Result<u8, CallError> {
        self.blocking(|p| p.volume()).await
    }
    async fn set_volume(&self, _: &Caller, l: u8) -> Result<(), CallError> {
        let l = level(l)?;
        self.blocking(move |p| p.set_volume(l)).await
    }
    async fn muted(&self, _: &Caller) -> Result<bool, CallError> {
        self.blocking(|p| p.muted()).await
    }
    async fn set_muted(&self, _: &Caller, m: bool) -> Result<(), CallError> {
        self.blocking(move |p| p.set_muted(m)).await
    }
    async fn brightness(&self, _: &Caller) -> Result<u8, CallError> {
        self.blocking(|p| p.brightness()).await
    }
    async fn set_brightness(&self, _: &Caller, l: u8) -> Result<(), CallError> {
        let l = level(l)?;
        self.blocking(move |p| p.set_brightness(l)).await
    }
    async fn wifi(&self, _: &Caller) -> Result<bool, CallError> {
        self.blocking(|p| p.wifi()).await
    }
    async fn set_wifi(&self, _: &Caller, on: bool) -> Result<(), CallError> {
        self.blocking(move |p| p.set_wifi(on)).await
    }
    async fn dark_mode(&self, _: &Caller) -> Result<bool, CallError> {
        self.blocking(|p| p.dark_mode()).await
    }
    async fn set_dark_mode(&self, _: &Caller, on: bool) -> Result<(), CallError> {
        self.blocking(move |p| p.set_dark_mode(on)).await
    }
    async fn focus_mode(&self, _: &Caller) -> Result<bool, CallError> {
        self.blocking(|p| p.focus_mode()).await
    }
    async fn set_focus_mode(&self, _: &Caller, on: bool) -> Result<(), CallError> {
        self.blocking(move |p| p.set_focus_mode(on)).await
    }
    async fn lock(&self, _: &Caller) -> Result<(), CallError> {
        self.blocking(|p| p.lock()).await
    }
    async fn sleep(&self, _: &Caller) -> Result<(), CallError> {
        self.blocking(|p| p.sleep()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::FakeRunner;

    fn device(os: Os, r: &FakeRunner) -> Device<FakeRunner> {
        Device::new(
            os,
            r.clone(),
            Box::new(NoNative),
            PathBuf::from("/nonexistent"),
        )
    }

    fn c() -> Caller {
        Caller::new("t")
    }

    #[test]
    fn parses_tool_output() {
        assert_eq!(parse_wpctl("Volume: 0.40"), Some((40, false)));
        assert_eq!(parse_wpctl("Volume: 1.00 [MUTED]\n"), Some((100, true)));
        assert_eq!(parse_wpctl("nope"), None);
        assert_eq!(
            parse_percent(
                "Volume: front-left: 26214 /  40% / -23.87 dB,   front-right: 26214 /  40%"
            ),
            Some(40)
        );
        assert_eq!(parse_percent("25%"), Some(25));
        assert_eq!(
            wifi_device(
                "Hardware Port: Ethernet\nDevice: en1\n\nHardware Port: Wi-Fi\nDevice: en0\nEthernet Address: x"
            ),
            Some("en0".into())
        );
        assert_eq!(
            wifi_device("Hardware Port: Thunderbolt\nDevice: bridge0"),
            None
        );
        assert!(mac_focus_on(
            r#"{"data":[{"storeAssertionRecords":[{"assertionDetails":{}}]}]}"#
        ));
        assert!(!mac_focus_on(r#"{"data":[{"storeAssertionRecords":[]}]}"#));
        assert!(!mac_focus_on("garbage"));
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }

    #[tokio::test]
    async fn macos_plans() {
        let r = FakeRunner::default();
        r.answer("/usr/bin/osascript -e output volume", "35")
            .answer("/usr/bin/osascript -e output muted", "false")
            .answer(
                "/usr/sbin/networksetup -listallhardwareports",
                "Hardware Port: Wi-Fi\nDevice: en0\n",
            )
            .answer(
                "/usr/sbin/networksetup -getairportpower en0",
                "Wi-Fi Power (en0): On",
            )
            .fail(
                "/usr/bin/defaults read -g AppleInterfaceStyle",
                1,
                "does not exist",
            )
            .answer("/usr/bin/shortcuts list", "Morning\nbricolo focus on\n");
        let d = device(Os::MacOs, &r);
        assert_eq!(d.volume(&c()).await.unwrap(), 35);
        assert!(!d.muted(&c()).await.unwrap());
        assert!(d.wifi(&c()).await.unwrap());
        assert!(!d.dark_mode(&c()).await.unwrap());
        d.set_volume(&c(), 70).await.unwrap();
        d.set_wifi(&c(), false).await.unwrap();
        d.set_dark_mode(&c(), true).await.unwrap();
        d.set_focus_mode(&c(), true).await.unwrap();
        let e = d.set_focus_mode(&c(), false).await.unwrap_err();
        assert!(matches!(e, CallError::Failed(m) if m.contains("\"bricolo focus off\"")));
        d.sleep(&c()).await.unwrap();
        // No native brightness or lock in this test.
        assert_eq!(d.brightness(&c()).await, Err(CallError::Unavailable));
        assert_eq!(d.lock(&c()).await, Err(CallError::Unavailable));
        assert!(d.set_volume(&c(), 101).await.is_err());
        let calls = r.calls();
        for want in [
            "/usr/bin/osascript -e set volume output volume 70",
            "/usr/sbin/networksetup -setairportpower en0 off",
            "/usr/bin/osascript -e tell application \"System Events\" to tell appearance preferences to set dark mode to true",
            "/usr/bin/shortcuts run bricolo focus on",
            "/usr/bin/pmset sleepnow",
        ] {
            assert!(calls.iter().any(|c| c == want), "{want} not in {calls:#?}");
        }
        assert!(!calls.iter().any(|c| c.contains("volume 101")));
    }

    #[tokio::test]
    async fn linux_plans_fall_back_from_wpctl_to_pactl() {
        let r = FakeRunner::default();
        r.error("wpctl", RunError::Missing)
            .answer(
                "pactl get-sink-volume",
                "Volume: front-left: 26214 /  40% / -23.87 dB",
            )
            .answer("pactl get-sink-mute", "Mute: yes")
            .answer("brightnessctl -m", "intel_backlight,backlight,240,25%,960")
            .answer("nmcli radio wifi", "disabled")
            .answer("gsettings get org.gnome.desktop.interface", "'prefer-dark'")
            .answer("gsettings get org.gnome.desktop.notifications", "false");
        let d = device(Os::Linux, &r);
        assert_eq!(d.volume(&c()).await.unwrap(), 40);
        assert!(d.muted(&c()).await.unwrap());
        assert_eq!(d.brightness(&c()).await.unwrap(), 25);
        assert!(!d.wifi(&c()).await.unwrap());
        assert!(d.dark_mode(&c()).await.unwrap());
        assert!(d.focus_mode(&c()).await.unwrap());
        d.set_volume(&c(), 55).await.unwrap();
        d.set_brightness(&c(), 80).await.unwrap();
        d.set_focus_mode(&c(), false).await.unwrap();
        d.lock(&c()).await.unwrap();
        d.sleep(&c()).await.unwrap();
        let calls = r.calls();
        for want in [
            "wpctl set-volume @DEFAULT_AUDIO_SINK@ 0.55",
            "pactl set-sink-volume @DEFAULT_SINK@ 55%",
            "brightnessctl set 80%",
            "gsettings set org.gnome.desktop.notifications show-banners true",
            "loginctl lock-session",
            "systemctl suspend",
        ] {
            assert!(calls.iter().any(|c| c == want), "{want} not in {calls:#?}");
        }

        let wp = FakeRunner::default();
        wp.answer("wpctl get-volume", "Volume: 0.62 [MUTED]");
        let d = device(Os::Linux, &wp);
        assert_eq!(d.volume(&c()).await.unwrap(), 62);
        assert!(d.muted(&c()).await.unwrap());

        let none = FakeRunner::default();
        none.error("", RunError::Missing);
        let e = device(Os::Linux, &none).volume(&c()).await.unwrap_err();
        assert_eq!(
            e,
            CallError::Failed("none of wpctl, pactl is installed".into())
        );
    }

    #[tokio::test]
    async fn windows_plans() {
        let r = FakeRunner::default();
        r.answer("powershell.exe", "40")
            .answer("reg query", "    AppsUseLightTheme    REG_DWORD    0x0");
        let d = device(Os::Windows, &r);
        assert_eq!(d.volume(&c()).await.unwrap(), 40);
        assert!(d.dark_mode(&c()).await.unwrap());
        d.set_dark_mode(&c(), false).await.unwrap();
        d.lock(&c()).await.unwrap();
        assert_eq!(d.focus_mode(&c()).await, Err(CallError::Unavailable));
        let calls = r.calls();
        assert!(calls[0].starts_with(
            "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand "
        ));
        assert!(calls.contains(&format!(
            "reg add {WIN_THEME} /v SystemUsesLightTheme /t REG_DWORD /d 1 /f"
        )));
        assert!(calls.contains(&"rundll32.exe user32.dll,LockWorkStation".to_string()));
    }

    struct FixedNative;
    impl Native for FixedNative {
        fn brightness(&self) -> Option<Result<u8, CallError>> {
            Some(Ok(64))
        }
        fn lock(&self) -> Option<Result<(), CallError>> {
            Some(Ok(()))
        }
    }

    #[tokio::test]
    async fn native_answers_come_first() {
        let r = FakeRunner::default();
        let d = Device::new(Os::MacOs, r.clone(), Box::new(FixedNative), PathBuf::new());
        assert_eq!(d.brightness(&c()).await.unwrap(), 64);
        d.lock(&c()).await.unwrap();
        assert!(r.calls().is_empty());
    }
}
