//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

use std::collections::HashMap;

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, PartialEq)]
pub struct Settings {
    pub every_ms: u32,
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let every_ms = serde_json::from_str::<serde_json::Value>(config)
            .ok()
            .and_then(|c| c["every_ms"].as_u64())
            .and_then(|ms| u32::try_from(ms).ok())
            .unwrap_or(10_000)
            .max(1_000);
        Settings { every_ms }
    }
}

/// "Battery 87%, on power"; none with no battery.
pub fn battery(battery: Option<u8>, plugged_in: bool) -> Option<String> {
    let detail = if plugged_in { "on power" } else { "on battery" };
    battery.map(|b| format!("Battery {b}%, {detail}"))
}

/// "Memory 62%, 20 of 32 GB" from vm_stat's output and hw.memsize. Used
/// memory is Activity Monitor's "Memory Used": app memory, wired and
/// compressed.
pub fn memory(vm_stat: &str, memsize: &str) -> Option<String> {
    let total = memsize.trim().parse::<f64>().ok().filter(|t| *t > 0.0)?;
    let mut page = 4096.0;
    let mut v = HashMap::new();
    for l in vm_stat.lines() {
        if let Some(size) = page_size(l) {
            page = size;
        }
        if let Some((key, n)) = counter(l) {
            v.insert(key, n);
        }
    }
    let get = |k: &str| v.get(k).copied().unwrap_or(0.0);
    let app = (get("Anonymous pages") - get("Pages purgeable")).max(0.0);
    let used =
        ((app + get("Pages wired down") + get("Pages occupied by compressor")) * page).min(total);
    Some(format!(
        "Memory {}%, {:.0} of {:.0} GB",
        (100.0 * used / total + 0.5).floor(),
        used / GB,
        total / GB
    ))
}

/// The page size in vm_stat's first line: "... (page size of 16384 bytes)".
fn page_size(l: &str) -> Option<f64> {
    let rest = &l[l.find("page size of ")? + "page size of ".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if !rest[digits.len()..].starts_with(" bytes") {
        return None;
    }
    digits.parse().ok()
}

/// `"Pages wired down":   123456.` as ("Pages wired down", 123456).
fn counter(l: &str) -> Option<(&str, f64)> {
    let (key, value) = l.split_once(':')?;
    let key = key.strip_prefix('"').unwrap_or(key);
    let key = key.strip_suffix('"').unwrap_or(key);
    if key.is_empty() || key.contains('"') || !value.starts_with(char::is_whitespace) {
        return None;
    }
    let n = value.trim_start().strip_suffix('.')?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((key, n.parse().ok()?))
}

/// "Disk 187 GB free" from `df -k`'s output: the fourth column of its
/// second line, in KiB.
pub fn disk(df: &str) -> Option<String> {
    let line = df.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let numeric = |s: &&str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if cols.len() < 4 || !cols[1..4].iter().all(numeric) {
        return None;
    }
    let available: f64 = cols[3].parse().ok()?;
    Some(format!("Disk {:.0} GB free", available * 1024.0 / 1e9))
}

/// The widget's rows from the parts that could be read: "Battery 87%, on
/// power" becomes the label "Battery" and the value "87%, on power". A Mac
/// with no battery gives no row there; the others still show.
pub fn rows(parts: &[Option<String>]) -> Vec<(String, String)> {
    parts
        .iter()
        .flatten()
        .map(|p| match p.split_once(' ') {
            Some((label, value)) => (label.to_string(), value.to_string()),
            None => (p.clone(), String::new()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VM_STAT: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                               12345.
Pages wired down:                        200000.
Pages purgeable:                          10000.
\"Translation faults\":                  9999999.
Anonymous pages:                         900000.
Pages occupied by compressor:            100000.
";

    const DF: &str =
        "Filesystem     1024-blocks      Used Available Capacity iused ifree %iused  Mounted on
/dev/disk3s1s1   971350180  10855320 182617460     6%  412510 1826174600    0%   /
";

    #[test]
    fn battery_reads_power_and_skips_desktops() {
        assert_eq!(
            battery(Some(87), true).as_deref(),
            Some("Battery 87%, on power")
        );
        assert_eq!(
            battery(Some(5), false).as_deref(),
            Some("Battery 5%, on battery")
        );
        assert_eq!(battery(None, true), None);
    }

    #[test]
    fn memory_counts_app_wired_and_compressed() {
        // (900000 - 10000 + 200000 + 100000) * 16384 bytes, of 32 GiB.
        let total = (32u64 << 30).to_string();
        assert_eq!(
            memory(VM_STAT, &total).as_deref(),
            Some("Memory 57%, 18 of 32 GB")
        );
    }

    #[test]
    fn memory_caps_used_at_total_and_needs_memsize() {
        let total = (8u64 << 30).to_string();
        assert_eq!(
            memory(VM_STAT, &total).as_deref(),
            Some("Memory 100%, 8 of 8 GB")
        );
        assert_eq!(memory(VM_STAT, ""), None);
        assert_eq!(memory(VM_STAT, "0"), None);
    }

    #[test]
    fn memory_defaults_to_4k_pages() {
        let vm = "Anonymous pages: 262144.\n";
        assert_eq!(
            memory(vm, &(4u64 << 30).to_string()).as_deref(),
            Some("Memory 25%, 1 of 4 GB")
        );
    }

    #[test]
    fn parses_vm_stat_lines() {
        assert_eq!(
            counter("Pages free:   12345."),
            Some(("Pages free", 12345.0))
        );
        assert_eq!(
            counter("\"Translation faults\":   7."),
            Some(("Translation faults", 7.0))
        );
        assert_eq!(counter("Mach Virtual Memory Statistics: (page size"), None);
        assert_eq!(page_size("x (page size of 16384 bytes)"), Some(16384.0));
        assert_eq!(page_size("Pages free: 1."), None);
    }

    #[test]
    fn disk_reads_available_kib() {
        assert_eq!(disk(DF).as_deref(), Some("Disk 187 GB free"));
        assert_eq!(disk("Filesystem\n"), None);
        assert_eq!(disk(""), None);
    }

    #[test]
    fn rows_split_the_label_from_what_was_read() {
        let row = |l: &str, v: &str| (l.to_string(), v.to_string());
        assert_eq!(
            rows(&[
                None,
                Some("Memory 1%".into()),
                Some("Disk 2 GB free".into())
            ]),
            [row("Memory", "1%"), row("Disk", "2 GB free")]
        );
        assert!(rows(&[None, None, None]).is_empty());
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("{}").every_ms, 10_000);
        assert_eq!(
            Settings::from_json(r#"{"every_ms":60000}"#).every_ms,
            60_000
        );
        assert_eq!(Settings::from_json(r#"{"every_ms":5}"#).every_ms, 1_000);
        assert_eq!(Settings::from_json("junk").every_ms, 10_000);
    }
}
