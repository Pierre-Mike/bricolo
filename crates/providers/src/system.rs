//! `system`: idle time, power, keeping the device awake, and system events
//! (sleep, wake, lock, screens). The macOS backend (`macos/system.rs`) reads
//! `ioreg` and `pmset`, runs `caffeinate`, and observes the workspace
//! notifications on the main-thread loop. The Linux backend
//! (`linux/system.rs`) reads `/sys/class/power_supply`, asks the X11
//! screen saver extension or logind for idle time, takes a logind inhibitor
//! lock to keep awake, and follows logind's sleep and lock signals and
//! RandR screen changes. The Windows backend (`windows/system.rs`) reads
//! `GetLastInputInfo` and `GetSystemPowerStatus`, holds
//! `SetThreadExecutionState` on a thread of its own, and listens to power,
//! session and display messages in a hidden window. Other systems answer
//! `CallError::Unavailable`.
//! `system` never runs on a peer.

use std::path::Path;

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Power {
    /// 0 to 100, or `None` without a battery.
    pub battery: Option<u8>,
    pub plugged_in: bool,
}

#[async_trait]
pub trait SystemProvider: Send + Sync {
    /// Seconds since the last key press, click or pointer move.
    async fn idle_seconds(&self, caller: &Caller) -> Result<u64, CallError>;
    async fn power(&self, caller: &Caller) -> Result<Power, CallError>;
    /// Keeps the device from idle sleep while `on`.
    async fn keep_awake(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    /// Starts or stops `system` events for the caller.
    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError>;
    /// Ends `plugin`'s keep-awake and its watch.
    fn release(&self, plugin: &str);
}

/// No system state on this node.
pub struct NoSystem;

#[async_trait]
impl SystemProvider for NoSystem {
    async fn idle_seconds(&self, _: &Caller) -> Result<u64, CallError> {
        Err(CallError::Unavailable)
    }

    async fn power(&self, _: &Caller) -> Result<Power, CallError> {
        Err(CallError::Unavailable)
    }

    async fn keep_awake(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn watch(&self, _: &Caller, _: bool) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    fn release(&self, _: &str) {}
}

/// Parses `pmset -g batt`: the first percentage is the battery, and the
/// power source line says whether the device is on mains.
pub fn parse_pmset(text: &str) -> Power {
    let plugged_in = text
        .lines()
        .find(|l| l.contains("Now drawing from"))
        .is_none_or(|l| l.contains("AC Power"));
    let battery = text.lines().find_map(|line| {
        let end = line.find('%')?;
        let digits: String = line[..end]
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits.parse::<u8>().ok().map(|n| n.min(100))
    });
    Power {
        battery,
        plugged_in,
    }
}

/// Parses `ioreg -c IOHIDSystem`: the first `"HIDIdleTime" = <ns>`, in
/// whole seconds.
pub fn parse_hid_idle(text: &str) -> Option<u64> {
    let rest = text.split("\"HIDIdleTime\"").nth(1)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let nanos: u64 = digits.parse().ok()?;
    Some(nanos / 1_000_000_000)
}

/// Reads a Linux power supply folder such as `/sys/class/power_supply`:
/// one subfolder per supply, each with a `type` file. The battery is the
/// mean `capacity` of the system batteries (`scope` other than `Device`,
/// which marks a mouse or a headset). The device is plugged in when a
/// `Mains`, `USB` or other supply is `online`; with no such supply, when no
/// battery is `Discharging`; and with nothing at all, as on a desktop or a
/// virtual machine, it is.
pub fn read_power_supply(dir: &Path) -> Power {
    let read = |path: &Path, name: &str| {
        std::fs::read_to_string(path.join(name))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let mut capacities = Vec::new();
    let mut discharging = false;
    let mut adapters = 0;
    let mut online = false;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for supply in entries {
        let Some(kind) = read(&supply, "type") else {
            continue;
        };
        if read(&supply, "scope").as_deref() == Some("Device") {
            continue;
        }
        if kind == "Battery" {
            if read(&supply, "present").as_deref() == Some("0") {
                continue;
            }
            if let Some(n) = read(&supply, "capacity").and_then(|s| s.parse::<u32>().ok()) {
                capacities.push(n.min(100));
            }
            discharging |= read(&supply, "status").as_deref() == Some("Discharging");
        } else if let Some(state) = read(&supply, "online") {
            adapters += 1;
            online |= state != "0";
        }
    }
    let battery = (!capacities.is_empty()).then(|| {
        let sum: u32 = capacities.iter().sum();
        let n = capacities.len() as u32;
        ((sum + n / 2) / n) as u8
    });
    let plugged_in = if adapters > 0 { online } else { !discharging };
    Power {
        battery,
        plugged_in,
    }
}

/// Seconds idle from logind's session `IdleHint` and `IdleSinceHint`
/// (microseconds since the epoch): zero while the session is not idle,
/// since the desktop only sets the hint after its own idle delay.
pub fn logind_idle_seconds(idle_hint: bool, since_usec: u64, now_usec: u64) -> u64 {
    if idle_hint {
        now_usec.saturating_sub(since_usec) / 1_000_000
    } else {
        0
    }
}

/// The event for a lock state seen now, given the last one seen, or
/// `None` when nothing changed. logind both asks the locker to lock (the
/// `Lock` signal) and reports the result (`LockedHint`), so the same change
/// can arrive twice.
pub fn lock_change(last: &mut Option<bool>, locked: bool) -> Option<crate::event::SystemEvent> {
    use crate::event::SystemEvent;
    if *last == Some(locked) {
        return None;
    }
    *last = Some(locked);
    Some(if locked {
        SystemEvent::ScreenLocked
    } else {
        SystemEvent::ScreenUnlocked
    })
}

/// Seconds idle on Windows from `GetTickCount` now and the tick of the
/// last input (`GetLastInputInfo`), both in milliseconds since boot and
/// wrapping every 49.7 days.
pub fn idle_from_ticks(now_ms: u32, last_input_ms: u32) -> u64 {
    u64::from(now_ms.wrapping_sub(last_input_ms)) / 1000
}

/// `SYSTEM_POWER_STATUS` as a [`Power`]. `ac_line` is 0 offline, 1 online,
/// 255 unknown; `battery_flag` 128 means no system battery and 255
/// unknown; `percent` 255 is unknown. Unknown mains counts as plugged in,
/// as on a desktop.
pub fn power_from_status(ac_line: u8, battery_flag: u8, percent: u8) -> Power {
    const NO_BATTERY: u8 = 128;
    const UNKNOWN: u8 = 255;
    let has_battery = battery_flag != UNKNOWN && battery_flag & NO_BATTERY == 0;
    let battery = (has_battery && percent != UNKNOWN).then(|| percent.min(100));
    Power {
        battery,
        plugged_in: ac_line != 0,
    }
}

/// Window messages and the values `system.watch` follows on Windows, as
/// winuser.h numbers them.
pub mod win_msg {
    pub const WM_DISPLAYCHANGE: u32 = 0x007E;
    pub const WM_POWERBROADCAST: u32 = 0x0218;
    pub const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
    pub const PBT_APMSUSPEND: usize = 0x0004;
    pub const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
    pub const WTS_SESSION_LOCK: usize = 0x7;
    pub const WTS_SESSION_UNLOCK: usize = 0x8;
}

/// The event for a window message, if `watch` reports it. A resume sends
/// `PBT_APMRESUMEAUTOMATIC` every time and `PBT_APMRESUMESUSPEND` as well
/// when a person woke the device, so only the first counts.
pub fn windows_message_event(msg: u32, wparam: usize) -> Option<crate::event::SystemEvent> {
    use crate::event::SystemEvent;
    use win_msg::*;
    match (msg, wparam) {
        (WM_POWERBROADCAST, PBT_APMSUSPEND) => Some(SystemEvent::WillSleep),
        (WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC) => Some(SystemEvent::DidWake),
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some(SystemEvent::ScreenLocked),
        (WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK) => Some(SystemEvent::ScreenUnlocked),
        (WM_DISPLAYCHANGE, _) => Some(SystemEvent::ScreensChanged),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            std::fs::write(dir.join(file), format!("{text}\n")).unwrap();
        }
    }

    #[test]
    fn power_supply_on_a_laptop_on_mains() {
        let root = tempfile::tempdir().unwrap();
        supply(root.path(), "AC", &[("type", "Mains"), ("online", "1")]);
        supply(
            root.path(),
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "87"),
                ("status", "Charging"),
            ],
        );
        // A USB-C port with nothing on it does not unplug the laptop. Windows
        // file names cannot hold the `:`, and only Linux reads this tree.
        #[cfg(not(windows))]
        supply(
            root.path(),
            "ucsi-source-psy-USBC000:001",
            &[("type", "USB"), ("online", "0")],
        );
        assert_eq!(
            read_power_supply(root.path()),
            Power {
                battery: Some(87),
                plugged_in: true
            }
        );
    }

    #[test]
    fn power_supply_on_battery() {
        let root = tempfile::tempdir().unwrap();
        supply(root.path(), "ADP1", &[("type", "Mains"), ("online", "0")]);
        supply(
            root.path(),
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "54"),
                ("status", "Discharging"),
            ],
        );
        supply(
            root.path(),
            "BAT1",
            &[
                ("type", "Battery"),
                ("capacity", "61"),
                ("status", "Discharging"),
            ],
        );
        assert_eq!(
            read_power_supply(root.path()),
            Power {
                battery: Some(58),
                plugged_in: false
            }
        );
    }

    #[test]
    fn power_supply_without_an_adapter_reads_the_battery_status() {
        let root = tempfile::tempdir().unwrap();
        supply(
            root.path(),
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "120"),
                ("status", "Discharging"),
            ],
        );
        assert_eq!(
            read_power_supply(root.path()),
            Power {
                battery: Some(100),
                plugged_in: false
            }
        );
        supply(
            root.path(),
            "BAT0",
            &[("type", "Battery"), ("capacity", "100"), ("status", "Full")],
        );
        assert!(read_power_supply(root.path()).plugged_in);
    }

    #[test]
    fn power_supply_ignores_peripherals_and_missing_batteries() {
        let root = tempfile::tempdir().unwrap();
        supply(
            root.path(),
            "hidpp_battery_0",
            &[
                ("type", "Battery"),
                ("scope", "Device"),
                ("capacity", "20"),
                ("status", "Discharging"),
            ],
        );
        supply(
            root.path(),
            "BAT1",
            &[("type", "Battery"), ("present", "0"), ("capacity", "0")],
        );
        let desktop = Power {
            battery: None,
            plugged_in: true,
        };
        assert_eq!(read_power_supply(root.path()), desktop);
        // No folder at all: a desktop, a container, or not Linux.
        assert_eq!(read_power_supply(&root.path().join("missing")), desktop);
    }

    #[test]
    fn logind_idle_counts_from_the_hint() {
        assert_eq!(logind_idle_seconds(false, 1_000_000, 90_000_000), 0);
        assert_eq!(logind_idle_seconds(true, 1_000_000, 91_500_000), 90);
        // A clock that went backwards.
        assert_eq!(logind_idle_seconds(true, 5_000_000, 1_000_000), 0);
    }

    #[test]
    fn lock_changes_fire_once() {
        use crate::event::SystemEvent;
        let mut last = None;
        assert_eq!(
            lock_change(&mut last, false),
            Some(SystemEvent::ScreenUnlocked)
        );
        assert_eq!(
            lock_change(&mut last, true),
            Some(SystemEvent::ScreenLocked)
        );
        // The `Lock` signal, then `LockedHint`: one event.
        assert_eq!(lock_change(&mut last, true), None);
        assert_eq!(
            lock_change(&mut last, false),
            Some(SystemEvent::ScreenUnlocked)
        );
    }

    #[test]
    fn pmset_reads_the_battery_and_the_source() {
        let on_ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t87%; charged; 0:00 remaining present: true\n";
        assert_eq!(
            parse_pmset(on_ac),
            Power {
                battery: Some(87),
                plugged_in: true
            }
        );
        let on_battery = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t54%; discharging; 3:12 remaining present: true\n";
        assert_eq!(
            parse_pmset(on_battery),
            Power {
                battery: Some(54),
                plugged_in: false
            }
        );
        let desktop = "Now drawing from 'AC Power'\n";
        assert_eq!(
            parse_pmset(desktop),
            Power {
                battery: None,
                plugged_in: true
            }
        );
    }

    #[test]
    fn windows_idle_survives_the_tick_wrap() {
        assert_eq!(idle_from_ticks(95_000, 5_000), 90);
        assert_eq!(idle_from_ticks(5_000, 5_000), 0);
        // GetTickCount wrapped after the last input.
        assert_eq!(idle_from_ticks(2_000, u32::MAX - 7_999), 10);
    }

    #[test]
    fn windows_power_status_maps_to_power() {
        // A laptop charging at 87%.
        assert_eq!(
            power_from_status(1, 8, 87),
            Power {
                battery: Some(87),
                plugged_in: true
            }
        );
        // On battery, low.
        assert_eq!(
            power_from_status(0, 2, 12),
            Power {
                battery: Some(12),
                plugged_in: false
            }
        );
        // A desktop: no system battery, mains unknown.
        let desktop = Power {
            battery: None,
            plugged_in: true,
        };
        assert_eq!(power_from_status(255, 128, 255), desktop);
        assert_eq!(power_from_status(1, 255, 255), desktop);
    }

    #[test]
    fn windows_messages_map_to_events() {
        use crate::event::SystemEvent;
        use win_msg::*;
        assert_eq!(
            windows_message_event(WM_POWERBROADCAST, PBT_APMSUSPEND),
            Some(SystemEvent::WillSleep)
        );
        assert_eq!(
            windows_message_event(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC),
            Some(SystemEvent::DidWake)
        );
        // PBT_APMRESUMESUSPEND follows a wake by hand: not a second event.
        assert_eq!(windows_message_event(WM_POWERBROADCAST, 0x7), None);
        assert_eq!(
            windows_message_event(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK),
            Some(SystemEvent::ScreenLocked)
        );
        assert_eq!(
            windows_message_event(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK),
            Some(SystemEvent::ScreenUnlocked)
        );
        // A remote session connecting is not a lock.
        assert_eq!(windows_message_event(WM_WTSSESSION_CHANGE, 0x3), None);
        assert_eq!(
            windows_message_event(WM_DISPLAYCHANGE, 32),
            Some(SystemEvent::ScreensChanged)
        );
        assert_eq!(windows_message_event(0x0010, 0), None);
    }

    #[test]
    fn hid_idle_time_is_in_whole_seconds() {
        let text = "+-o IOHIDSystem  <class IOHIDSystem>\n    {\n      \"HIDIdleTime\" = 12999999999\n    }\n";
        assert_eq!(parse_hid_idle(text), Some(12));
        assert_eq!(parse_hid_idle("nothing here"), None);
    }
}
