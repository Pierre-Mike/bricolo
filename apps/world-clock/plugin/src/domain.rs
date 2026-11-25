//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

/// West to east, which is also the order the tile shows them in.
pub const CITIES: &[(&str, &str)] = &[
    ("Honolulu", "Pacific/Honolulu"),
    ("Seattle", "America/Los_Angeles"),
    ("Denver", "America/Denver"),
    ("Chicago", "America/Chicago"),
    ("New York", "America/New_York"),
    ("São Paulo", "America/Sao_Paulo"),
    ("London", "Europe/London"),
    ("Paris", "Europe/Paris"),
    ("Dubai", "Asia/Dubai"),
    ("Mumbai", "Asia/Kolkata"),
    ("Singapore", "Asia/Singapore"),
    ("Tokyo", "Asia/Tokyo"),
    ("Sydney", "Australia/Sydney"),
];

pub const DEFAULT: &str = "Seattle,New York,Paris";

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
            .unwrap_or(30_000)
            .max(1_000);
        Settings { every_ms }
    }
}

/// The cities named in `s` ("Seattle,Paris"), in list order.
pub fn parse(s: &str) -> Vec<(&'static str, &'static str)> {
    let picked: Vec<&str> = s.split(',').map(str::trim).collect();
    CITIES
        .iter()
        .copied()
        .filter(|(name, _)| picked.contains(name))
        .collect()
}

/// `s` with `name` switched on or off.
pub fn toggled(s: &str, name: &str, on: bool) -> String {
    let was = parse(s);
    CITIES
        .iter()
        .filter(|(c, _)| {
            if *c == name {
                on
            } else {
                was.iter().any(|(p, _)| p == c)
            }
        })
        .map(|(c, _)| *c)
        .collect::<Vec<_>>()
        .join(",")
}

/// The widget's rows, one city and its time each, `?` for a time that
/// could not be read. None when no city is picked.
pub fn rows(cities: &[(&str, &str)], times: &[Option<String>]) -> Vec<(String, String)> {
    cities
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            let t = times.get(i).cloned().flatten();
            (name.to_string(), t.unwrap_or_else(|| "?".into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_in_list_order_and_ignores_unknown_names() {
        let names: Vec<_> = parse(" Paris ,Seattle,Atlantis")
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(names, ["Seattle", "Paris"]);
        assert!(parse("").is_empty());
    }

    #[test]
    fn toggles_a_city_and_keeps_the_order() {
        assert_eq!(
            toggled(DEFAULT, "Tokyo", true),
            "Seattle,New York,Paris,Tokyo"
        );
        assert_eq!(toggled(DEFAULT, "New York", false), "Seattle,Paris");
        assert_eq!(toggled("", "London", true), "London");
        assert_eq!(toggled("London", "London", false), "");
    }

    #[test]
    fn one_row_per_city_with_missing_times() {
        let cities = parse("Seattle,Paris");
        let row = |c: &str, t: &str| (c.to_string(), t.to_string());
        assert_eq!(
            rows(&cities, &[Some("00:38".into()), None]),
            [row("Seattle", "00:38"), row("Paris", "?")]
        );
        assert!(rows(&[], &[]).is_empty());
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        assert_eq!(Settings::from_json("{}").every_ms, 30_000);
        assert_eq!(
            Settings::from_json(r#"{"every_ms":60000}"#).every_ms,
            60_000
        );
        assert_eq!(Settings::from_json(r#"{"every_ms":5}"#).every_ms, 1_000);
        assert_eq!(Settings::from_json("junk").every_ms, 30_000);
    }
}
