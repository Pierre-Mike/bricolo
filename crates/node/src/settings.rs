//! The hub's Settings tab: a plugin's `[settings]` defaults, its
//! `[settings_ui]` hints, and the overrides in `[plugins.<name>]` of
//! node.toml.
//!
//! `{"cmd":"settings","plugin":"p"}` answers [`describe`];
//! `{"cmd":"set-settings","plugin":"p","values":{"key":value}}` runs
//! [`edit`] on node.toml and reloads the plugin. A `null` value, or the
//! default itself, removes the override, so node.toml only holds what you
//! changed. Edits go through `toml_edit`, as `bricolo sidecar` does: comments
//! and order outside `[plugins.<name>]` stay as they were.

use bricolo_runtime::manifest::{Manifest, SettingUi};
use bricolo_runtime::node_config::NodeConfig;
use serde_json::{Map, Value, json};
use toml_edit::{Array, DocumentMut, Item, Table, value};

/// How the hub draws one setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Integer,
    Float,
    /// A string like "18:30".
    Time,
    Text,
    /// A string from `choices`.
    Choice,
    /// A list of strings from `choices`.
    Multi,
    /// A list of strings.
    List,
    /// Tables, dates and mixed lists: shown, not edited.
    Other,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::Integer => "integer",
            Kind::Float => "float",
            Kind::Time => "time",
            Kind::Text => "text",
            Kind::Choice => "choice",
            Kind::Multi => "multi",
            Kind::List => "list",
            Kind::Other => "other",
        }
    }
}

pub fn kind(default: &toml::Value, ui: &SettingUi) -> Kind {
    let choices = !ui.choices.is_empty();
    match default {
        toml::Value::Boolean(_) => Kind::Bool,
        toml::Value::Integer(_) => Kind::Integer,
        toml::Value::Float(_) => Kind::Float,
        toml::Value::String(_) if choices => Kind::Choice,
        toml::Value::String(s) if is_time(s) => Kind::Time,
        toml::Value::String(_) => Kind::Text,
        toml::Value::Array(items) if items.iter().all(toml::Value::is_str) => {
            if choices {
                Kind::Multi
            } else {
                Kind::List
            }
        }
        _ => Kind::Other,
    }
}

/// "HH:MM", 00:00 to 23:59.
fn is_time(s: &str) -> bool {
    let Some((h, m)) = s.split_once(':') else {
        return false;
    };
    h.len() == 2
        && m.len() == 2
        && h.parse::<u8>().is_ok_and(|h| h < 24)
        && m.parse::<u8>().is_ok_and(|m| m < 60)
}

/// Paths and URLs go under Advanced, unless `[settings_ui]` says
/// otherwise or gives them a label.
fn advanced(default: &toml::Value, ui: &SettingUi) -> bool {
    ui.advanced.unwrap_or_else(|| {
        ui.label.is_none()
            && default.as_str().is_some_and(|s| {
                s.starts_with('/')
                    || s.starts_with('~')
                    || s.starts_with("http://")
                    || s.starts_with("https://")
            })
    })
}

/// The keys of `[settings]` in the order plugin.toml lists them. `toml`
/// sorts its tables, and the author's order reads better.
pub fn order(plugin_toml: &str, manifest: &Manifest) -> Vec<String> {
    let mut keys: Vec<String> = plugin_toml
        .parse::<DocumentMut>()
        .ok()
        .and_then(|doc| {
            doc.get("settings")
                .and_then(Item::as_table_like)
                .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
        })
        .unwrap_or_default();
    keys.retain(|k| manifest.settings.contains_key(k));
    for k in manifest.settings.keys() {
        if !keys.contains(k) {
            keys.push(k.clone());
        }
    }
    keys
}

/// The answer to `{"cmd":"settings"}`.
pub fn describe(manifest: &Manifest, order: &[String], overrides: Option<&toml::Table>) -> Value {
    let none = SettingUi::default();
    let settings: Vec<Value> = order
        .iter()
        .filter_map(|key| {
            let default = manifest.settings.get(key)?;
            let ui = manifest.settings_ui.get(key).unwrap_or(&none);
            let set = overrides.and_then(|o| o.get(key));
            Some(json!({
                "key": key,
                "label": ui.label,
                "help": ui.help,
                "kind": kind(default, ui).name(),
                "choices": ui.choices,
                "advanced": advanced(default, ui),
                "hidden": ui.hidden,
                "default": default,
                "value": set.unwrap_or(default),
                "overridden": set.is_some(),
            }))
        })
        .collect();
    json!({ "plugin": manifest.name, "settings": settings })
}

/// node.toml with `changes` applied to `[plugins.<name>]`, checked as a
/// whole `NodeConfig`.
pub fn edit(
    text: &str,
    manifest: &Manifest,
    changes: &Map<String, Value>,
) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("node.toml is not valid TOML: {e}"))?;
    let name = &manifest.name;
    let plugins = doc.entry("plugins").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let plugins = plugins
        .as_table_like_mut()
        .ok_or("`plugins` in node.toml must be a table")?;
    let section = plugins
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| format!("[plugins.{name}] in node.toml must be a table"))?;
    let none = SettingUi::default();
    for (key, new) in changes {
        let Some(default) = manifest.settings.get(key) else {
            return Err(format!("{name} has no setting {key:?}"));
        };
        let ui = manifest.settings_ui.get(key).unwrap_or(&none);
        if new.is_null() {
            section.remove(key);
            continue;
        }
        let new = to_toml(key, default, ui, new)?;
        if new == *default {
            section.remove(key);
        } else {
            section.insert(key, to_item(&new));
        }
    }
    if section.is_empty() {
        plugins.remove(name);
    }
    if plugins.is_empty() {
        doc.remove("plugins");
    }
    let out = doc.to_string();
    let config: NodeConfig =
        toml::from_str(&out).map_err(|e| format!("node.toml would be invalid: {e}"))?;
    config
        .validate()
        .map_err(|e| format!("node.toml would be invalid: {e}"))?;
    Ok(out)
}

/// The JSON the hub sent, as a TOML value of the default's type.
fn to_toml(
    key: &str,
    default: &toml::Value,
    ui: &SettingUi,
    new: &Value,
) -> Result<toml::Value, String> {
    let wrong = |what: &str| format!("{key} must be {what}");
    let chosen = |s: &str| ui.choices.is_empty() || ui.choices.iter().any(|c| c == s);
    match kind(default, ui) {
        Kind::Bool => new
            .as_bool()
            .map(toml::Value::Boolean)
            .ok_or_else(|| wrong("true or false")),
        Kind::Integer => new
            .as_i64()
            .map(toml::Value::Integer)
            .ok_or_else(|| wrong("a whole number")),
        Kind::Float => new
            .as_f64()
            .map(toml::Value::Float)
            .ok_or_else(|| wrong("a number")),
        Kind::Time => match new.as_str() {
            Some(s) if is_time(s) => Ok(toml::Value::String(s.into())),
            _ => Err(wrong("a time like 18:30")),
        },
        Kind::Text => new
            .as_str()
            .map(|s| toml::Value::String(s.into()))
            .ok_or_else(|| wrong("text")),
        Kind::Choice => match new.as_str() {
            Some(s) if chosen(s) => Ok(toml::Value::String(s.into())),
            _ => Err(wrong(&format!("one of {}", ui.choices.join(", ")))),
        },
        Kind::Multi | Kind::List => {
            let items = new.as_array().ok_or_else(|| wrong("a list of text"))?;
            let mut out = Vec::new();
            for item in items {
                match item.as_str() {
                    Some(s) if chosen(s) => out.push(toml::Value::String(s.into())),
                    Some(s) => return Err(format!("{key}: {s:?} is not one of the choices")),
                    None => return Err(wrong("a list of text")),
                }
            }
            Ok(toml::Value::Array(out))
        }
        Kind::Other => Err(format!(
            "{key} cannot be changed in the hub; edit node.toml by hand"
        )),
    }
}

fn to_item(v: &toml::Value) -> Item {
    match v {
        toml::Value::Boolean(b) => value(*b),
        toml::Value::Integer(i) => value(*i),
        toml::Value::Float(f) => value(*f),
        toml::Value::String(s) => value(s.as_str()),
        toml::Value::Array(items) => value(
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Array>(),
        ),
        _ => Item::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUGIN: &str = r#"
name = "voice"
version = "0.1.0"

[settings]
voice = "alba"
retro_at = "18:30"
turns = 50
review = true
cities = ["Tokyo"]
uv = "~/.local/bin/uv"
chord = "ctrl+alt+v"

[settings_ui.voice]
label = "Voice"
choices = ["alba", "marius"]

[settings_ui.cities]
choices = ["Tokyo", "Paris", "London"]

[settings_ui.chord]
hidden = true
"#;

    fn manifest() -> Manifest {
        Manifest::parse(PLUGIN).unwrap()
    }

    fn changes(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn describes_in_file_order_with_kinds_and_overrides() {
        let m = manifest();
        let order = order(PLUGIN, &m);
        assert_eq!(
            order,
            [
                "voice", "retro_at", "turns", "review", "cities", "uv", "chord"
            ]
        );
        let overrides: toml::Table = toml::from_str("turns = 20").unwrap();
        let d = describe(&m, &order, Some(&overrides));
        let s = d["settings"].as_array().unwrap();
        let kinds: Vec<&str> = s.iter().map(|x| x["kind"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            ["choice", "time", "integer", "bool", "multi", "text", "text"]
        );
        assert_eq!(s[2]["value"], 20);
        assert_eq!(s[2]["default"], 50);
        assert_eq!(s[2]["overridden"], true);
        assert_eq!(s[0]["overridden"], false);
        assert_eq!(s[5]["advanced"], true, "a path is advanced");
        assert_eq!(s[6]["hidden"], true);
    }

    #[test]
    fn edit_writes_only_what_differs_and_keeps_comments() {
        let m = manifest();
        let text =
            "# my node\nname = \"mac\"\n\n[sidecars.a]\n# keep me\ncommand = \"x\"\nport = 1\n";
        let out = edit(
            text,
            &m,
            &changes(json!({ "voice": "marius", "retro_at": "07:15", "review": true, "cities": ["Paris"] })),
        )
        .unwrap();
        assert!(
            out.contains("# my node") && out.contains("# keep me"),
            "{out}"
        );
        let config: NodeConfig = toml::from_str(&out).unwrap();
        let set = &config.plugins["voice"];
        assert_eq!(set["voice"].as_str(), Some("marius"));
        assert_eq!(set["retro_at"].as_str(), Some("07:15"));
        assert!(!set.contains_key("review"), "the default is not written");
        assert_eq!(set["cities"].as_array().unwrap().len(), 1);

        // Back to the defaults: the section goes away.
        let back = edit(
            &out,
            &m,
            &changes(json!({ "voice": null, "retro_at": "18:30", "cities": ["Tokyo"] })),
        )
        .unwrap();
        assert!(!back.contains("[plugins"), "{back}");
        assert!(back.contains("# keep me"));
    }

    #[test]
    fn edit_refuses_wrong_types_unknown_keys_and_foreign_choices() {
        let m = manifest();
        let bad = |v: Value| edit("", &m, &changes(v)).unwrap_err();
        assert!(bad(json!({ "turns": "many" })).contains("whole number"));
        assert!(bad(json!({ "review": 1 })).contains("true or false"));
        assert!(bad(json!({ "retro_at": "25:00" })).contains("time"));
        assert!(bad(json!({ "voice": "bob" })).contains("one of alba, marius"));
        assert!(bad(json!({ "cities": ["Rome"] })).contains("not one of"));
        assert!(bad(json!({ "nope": 1 })).contains("no setting"));
    }

    #[test]
    fn is_time_takes_hours_and_minutes_only() {
        assert!(is_time("00:00") && is_time("23:59"));
        assert!(!is_time("24:00") && !is_time("7:15") && !is_time("ab:cd") && !is_time("12:60"));
    }
}
