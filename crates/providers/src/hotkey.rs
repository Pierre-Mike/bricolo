//! `hotkey`: global shortcuts. The macOS backend lives in `macos/hotkey.rs`
//! and needs the main-thread loop (see `desktop`). Chord parsing is here,
//! so every backend reads chords the same way.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::caller::Caller;
use crate::error::CallError;

#[async_trait]
pub trait HotkeyProvider: Send + Sync {
    /// Binds `chord` (like "cmd+alt+h") for the caller under `id`. Presses
    /// arrive as `ProviderEvent::Hotkey(id)` through the event sink, and
    /// releases, where the backend sees them, as `HotkeyReleased(id)`.
    async fn bind(&self, caller: &Caller, id: String, chord: String) -> Result<(), CallError>;
    async fn unbind(&self, caller: &Caller, id: String) -> Result<(), CallError>;
    /// Drops every binding of `plugin`.
    fn release(&self, plugin: &str);
    /// `plugin`'s bindings as `(id, chord)`, sorted by id.
    fn bound(&self, _plugin: &str) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The bindings of `plugin` that the user moved to another chord, as
    /// `(id, chord the plugin asked for)`, sorted by id. See [`Remap`].
    fn changed(&self, _plugin: &str) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Moves `plugin`'s binding `id` to `chord`, or back to the chord the
    /// plugin asked for when `chord` is `None`. Answers the chord now bound.
    async fn rebind(
        &self,
        _plugin: &str,
        _id: &str,
        _chord: Option<String>,
    ) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
}

/// What the user changed: for each plugin or script, the chord it asks
/// for and the chord bound instead. `<data>/hotkeys.toml` holds it:
///
/// ```toml
/// ["dictation.lua"]
/// "ctrl+alt+d" = "cmd+shift+d"
/// ```
type Chords = BTreeMap<String, BTreeMap<String, String>>;

/// Wraps a backend so the user can move any binding to another chord, from
/// the hub, without the plugin or script knowing. A plugin keeps asking for
/// its own chord; `Remap` binds the user's chord in its place. Changes are
/// matched by the chord asked for, not the id, because a script's ids
/// (`h1`, `h2`) follow the order of its `bind` calls.
pub struct Remap {
    inner: Arc<dyn HotkeyProvider>,
    file: Option<PathBuf>,
    chords: Mutex<Chords>,
    /// What each plugin asked for: plugin, then id, then chord.
    asked: Mutex<Chords>,
}

impl Remap {
    /// Reads `file` when given; a missing or broken file changes nothing.
    pub fn new(inner: Arc<dyn HotkeyProvider>, file: Option<PathBuf>) -> Self {
        let chords = file.as_deref().map(load).unwrap_or_default();
        Self {
            inner,
            file,
            chords: Mutex::new(chords),
            asked: Mutex::default(),
        }
    }

    /// The chord to bind when `plugin` asks for `asked`.
    fn effective(&self, plugin: &str, asked: &str) -> String {
        let chords = self.chords.lock().unwrap();
        chords
            .get(plugin)
            .and_then(|map| map.iter().find(|(from, _)| same(from, asked)))
            .map_or_else(|| asked.to_string(), |(_, to)| to.clone())
    }

    fn save(&self) {
        let Some(file) = &self.file else { return };
        let chords = self.chords.lock().unwrap().clone();
        if let Err(e) = write(file, &chords) {
            tracing::warn!("cannot save {}: {e}", file.display());
        }
    }
}

/// Whether two chords name the same keys, however they are written.
fn same(a: &str, b: &str) -> bool {
    match (Chord::parse(a), Chord::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn load(file: &Path) -> Chords {
    match std::fs::read_to_string(file) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{}: {e}; using the chords plugins ask for", file.display());
            Chords::new()
        }),
        Err(_) => Chords::new(),
    }
}

/// A temporary file and a rename, so a crash never leaves half a file.
fn write(file: &Path, chords: &Chords) -> std::io::Result<()> {
    let body = toml::to_string(chords).map_err(std::io::Error::other)?;
    let text = format!(
        "# Written by bricolo: chords changed in the hub. For each plugin or script,\n\
         # the chord it asks for = the chord bound instead.\n{body}"
    );
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

#[async_trait]
impl HotkeyProvider for Remap {
    async fn bind(&self, caller: &Caller, id: String, chord: String) -> Result<(), CallError> {
        let effective = self.effective(&caller.plugin, &chord);
        self.asked
            .lock()
            .unwrap()
            .entry(caller.plugin.clone())
            .or_default()
            .insert(id.clone(), chord.clone());
        let result = self.inner.bind(caller, id, effective.clone()).await;
        if let Err(e) = &result
            && effective != chord
        {
            tracing::warn!(plugin = %caller.plugin, "{chord} is set to {effective} in the hub, which failed: {e}");
        }
        result
    }

    async fn unbind(&self, caller: &Caller, id: String) -> Result<(), CallError> {
        if let Some(map) = self.asked.lock().unwrap().get_mut(&caller.plugin) {
            map.remove(&id);
        }
        self.inner.unbind(caller, id).await
    }

    fn release(&self, plugin: &str) {
        self.asked.lock().unwrap().remove(plugin);
        self.inner.release(plugin);
    }

    fn bound(&self, plugin: &str) -> Vec<(String, String)> {
        self.inner.bound(plugin)
    }

    fn changed(&self, plugin: &str) -> Vec<(String, String)> {
        let asked = self.asked.lock().unwrap().get(plugin).cloned();
        asked
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, chord)| !same(&self.effective(plugin, chord), chord))
            .collect()
    }

    async fn rebind(
        &self,
        plugin: &str,
        id: &str,
        chord: Option<String>,
    ) -> Result<String, CallError> {
        let asked = self
            .asked
            .lock()
            .unwrap()
            .get(plugin)
            .and_then(|map| map.get(id))
            .cloned()
            .ok_or_else(|| CallError::Failed(format!("{plugin} has no hotkey {id} bound")))?;
        let new = match chord {
            Some(c) => {
                let parsed = Chord::parse(&c).map_err(CallError::Failed)?;
                if parsed.is_fn() {
                    return Err(CallError::Failed(
                        "fn is for push-to-talk; pick a chord with a key".into(),
                    ));
                }
                c.trim().to_ascii_lowercase()
            }
            None => asked.clone(),
        };
        let old = self.effective(plugin, &asked);
        if same(&old, &new) {
            return Ok(old);
        }
        let caller = Caller::new(plugin);
        if let Err(e) = self.inner.bind(&caller, id.to_string(), new.clone()).await {
            // A backend that let go of the old chord before failing gets it
            // back; when the old chord is still bound this does nothing.
            let _ = self.inner.bind(&caller, id.to_string(), old).await;
            return Err(e);
        }
        {
            let mut chords = self.chords.lock().unwrap();
            let map = chords.entry(plugin.to_string()).or_default();
            map.retain(|from, _| !same(from, &asked));
            if !same(&new, &asked) {
                map.insert(asked, new.clone());
            }
            if map.is_empty() {
                chords.remove(plugin);
            }
        }
        self.save();
        Ok(new)
    }
}

/// No hotkeys on this node.
pub struct NoHotkeys;

#[async_trait]
impl HotkeyProvider for NoHotkeys {
    async fn bind(&self, _: &Caller, _: String, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    async fn unbind(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }

    fn release(&self, _: &str) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Cmd,
}

/// A parsed chord: modifiers, sorted and without repeats, and a key given
/// by its W3C `KeyboardEvent.code` name ("KeyH", "ArrowLeft", "F5").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: Vec<Modifier>,
    pub code: String,
}

impl Chord {
    /// Parses "cmd+alt+left". Modifiers: cmd (command, super), ctrl
    /// (control), alt (opt, option), shift. Keys: a to z, 0 to 9, f1 to
    /// f20, left, right, up, down, space, enter (return), tab, escape (esc),
    /// delete (backspace), forwarddelete, home, end, pageup, pagedown, and
    /// the punctuation - = [ ] \ ; ' , . / `. `fn` (the Fn or Globe key)
    /// is a chord alone, with no modifiers; only macOS binds it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let lower = text.trim().to_ascii_lowercase();
        // "cmd++" names the key that types + and =.
        let (head, key) = match lower.strip_suffix("++") {
            Some(head) => (head, "+"),
            None => lower.rsplit_once('+').unwrap_or(("", lower.as_str())),
        };
        Self::build(text, head.split('+'), key)
    }

    /// The chord of a modifier list and a key, as `keyboard.keystroke`
    /// takes them: `(["cmd"], "v")`. Names are those of [`Chord::parse`].
    pub fn from_parts(mods: &[String], key: &str) -> Result<Self, String> {
        let text = format!("{}+{key}", mods.join("+"));
        let lower: Vec<String> = mods.iter().map(|m| m.trim().to_ascii_lowercase()).collect();
        Self::build(
            &text,
            lower.iter().map(String::as_str),
            &key.trim().to_ascii_lowercase(),
        )
    }

    fn build<'a>(
        text: &str,
        names: impl Iterator<Item = &'a str>,
        key: &str,
    ) -> Result<Self, String> {
        let mut mods = Vec::new();
        for m in names.filter(|m| !m.is_empty()) {
            mods.push(match m {
                "cmd" | "command" | "super" | "meta" => Modifier::Cmd,
                "ctrl" | "control" => Modifier::Ctrl,
                "alt" | "opt" | "option" => Modifier::Alt,
                "shift" => Modifier::Shift,
                other => return Err(format!("{text:?}: unknown modifier {other:?}")),
            });
        }
        mods.sort();
        mods.dedup();
        let code = key_code(key).ok_or_else(|| format!("{text:?}: unknown key {key:?}"))?;
        if code == FN && !mods.is_empty() {
            return Err(format!("{text:?}: fn takes no modifiers"));
        }
        Ok(Self { mods, code })
    }

    /// Whether this is the Fn key alone.
    pub fn is_fn(&self) -> bool {
        self.code == FN
    }
}

/// The code of the Fn (Globe) key. W3C names it "Fn" too.
pub const FN: &str = "Fn";

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for m in &self.mods {
            let name = match m {
                Modifier::Cmd => "cmd",
                Modifier::Ctrl => "ctrl",
                Modifier::Alt => "alt",
                Modifier::Shift => "shift",
            };
            write!(f, "{name}+")?;
        }
        f.write_str(&self.code)
    }
}

/// The W3C code name for a lowercase key name.
fn key_code(key: &str) -> Option<String> {
    let named = match key {
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "space" => "Space",
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "escape" | "esc" => "Escape",
        "delete" | "backspace" => "Backspace",
        "forwarddelete" => "Delete",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "-" => "Minus",
        "=" | "+" => "Equal",
        "[" => "BracketLeft",
        "]" => "BracketRight",
        "\\" => "Backslash",
        ";" => "Semicolon",
        "'" => "Quote",
        "," => "Comma",
        "." => "Period",
        "/" => "Slash",
        "`" => "Backquote",
        "fn" | "globe" => FN,
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    let bytes = key.as_bytes();
    match bytes {
        [c] if c.is_ascii_lowercase() => Some(format!("Key{}", key.to_ascii_uppercase())),
        [c] if c.is_ascii_digit() => Some(format!("Digit{key}")),
        [b'f', ..] => {
            let n: u8 = key[1..].parse().ok()?;
            (1..=20).contains(&n).then(|| format!("F{n}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_keys() {
        let c = Chord::parse("cmd+alt+left").unwrap();
        assert_eq!(c.mods, [Modifier::Alt, Modifier::Cmd]);
        assert_eq!(c.code, "ArrowLeft");
        assert_eq!(Chord::parse("Option+Command+H").unwrap(), {
            Chord {
                mods: vec![Modifier::Alt, Modifier::Cmd],
                code: "KeyH".into(),
            }
        });
        assert_eq!(Chord::parse("ctrl+shift+7").unwrap().code, "Digit7");
        assert_eq!(Chord::parse("f12").unwrap().mods, []);
        assert_eq!(Chord::parse("cmd+/").unwrap().code, "Slash");
        assert_eq!(Chord::parse("cmd++").unwrap().code, "Equal");
        assert_eq!(Chord::parse("cmd+cmd+a").unwrap().mods, [Modifier::Cmd]);
    }

    #[test]
    fn rejects_unknown_parts() {
        assert!(Chord::parse("hyper+a").is_err());
        assert!(Chord::parse("cmd+f21").is_err());
        assert!(Chord::parse("cmd+pgup").is_err());
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn fn_is_a_chord_alone() {
        let c = Chord::parse("Fn").unwrap();
        assert!(c.is_fn() && c.mods.is_empty());
        assert_eq!(Chord::parse("globe").unwrap(), c);
        assert!(Chord::parse("cmd+fn").is_err());
        assert!(!Chord::parse("f1").unwrap().is_fn());
    }

    #[test]
    fn builds_from_a_modifier_list_and_a_key() {
        let mods = |list: &[&str]| list.iter().map(|m| m.to_string()).collect::<Vec<_>>();
        assert_eq!(
            Chord::from_parts(&mods(&["Cmd"]), "V").unwrap(),
            Chord::parse("cmd+v").unwrap()
        );
        assert_eq!(Chord::from_parts(&[], "+").unwrap().code, "Equal");
        assert_eq!(Chord::from_parts(&[], "enter").unwrap().mods, []);
        assert!(Chord::from_parts(&mods(&["hyper"]), "v").is_err());
        assert!(Chord::from_parts(&mods(&["cmd"]), "").is_err());
    }

    /// Records bindings as (plugin, id) -> chord, and refuses a chord
    /// another binding holds, like the real backends.
    #[derive(Default)]
    struct Fake(Mutex<BTreeMap<(String, String), String>>);

    #[async_trait]
    impl HotkeyProvider for Fake {
        async fn bind(&self, c: &Caller, id: String, chord: String) -> Result<(), CallError> {
            let mut t = self.0.lock().unwrap();
            let key = (c.plugin.clone(), id);
            if let Some((holder, _)) = t.iter().find(|(k, v)| same(v, &chord) && **k != key) {
                return Err(CallError::Failed(format!(
                    "{chord} is already bound by {}",
                    holder.0
                )));
            }
            t.insert(key, chord);
            Ok(())
        }
        async fn unbind(&self, c: &Caller, id: String) -> Result<(), CallError> {
            self.0.lock().unwrap().remove(&(c.plugin.clone(), id));
            Ok(())
        }
        fn release(&self, plugin: &str) {
            self.0.lock().unwrap().retain(|(p, _), _| p != plugin);
        }
        fn bound(&self, plugin: &str) -> Vec<(String, String)> {
            let t = self.0.lock().unwrap();
            t.iter()
                .filter(|((p, _), _)| p == plugin)
                .map(|((_, id), chord)| (id.clone(), chord.clone()))
                .collect()
        }
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn rebinds_and_remembers_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hotkeys.toml");
        let remap = Remap::new(Arc::new(Fake::default()), Some(file.clone()));
        let caller = Caller::new("dictation.lua");
        remap
            .bind(&caller, "h1".into(), "ctrl+alt+d".into())
            .await
            .unwrap();
        assert!(remap.changed("dictation.lua").is_empty());

        let now = remap
            .rebind("dictation.lua", "h1", Some("Cmd+Shift+D".into()))
            .await
            .unwrap();
        assert_eq!(now, "cmd+shift+d");
        assert_eq!(
            remap.bound("dictation.lua"),
            pairs(&[("h1", "cmd+shift+d")])
        );
        assert_eq!(
            remap.changed("dictation.lua"),
            pairs(&[("h1", "ctrl+alt+d")])
        );

        // A new node reads the file: the script asks for its chord, written
        // another way, and gets the user's.
        let again = Remap::new(Arc::new(Fake::default()), Some(file.clone()));
        again
            .bind(&caller, "h1".into(), "alt+ctrl+d".into())
            .await
            .unwrap();
        assert_eq!(
            again.bound("dictation.lua"),
            pairs(&[("h1", "cmd+shift+d")])
        );

        // Back to the script's own chord: the file forgets it.
        assert_eq!(
            again.rebind("dictation.lua", "h1", None).await.unwrap(),
            "alt+ctrl+d"
        );
        assert_eq!(again.bound("dictation.lua"), pairs(&[("h1", "alt+ctrl+d")]));
        assert!(again.changed("dictation.lua").is_empty());
        assert!(
            !std::fs::read_to_string(&file)
                .unwrap()
                .contains("dictation")
        );
    }

    #[tokio::test]
    async fn refuses_what_it_cannot_bind_and_keeps_the_old_chord() {
        let remap = Remap::new(Arc::new(Fake::default()), None);
        remap
            .bind(&Caller::new("a"), "go".into(), "cmd+1".into())
            .await
            .unwrap();
        remap
            .bind(&Caller::new("b"), "go".into(), "cmd+2".into())
            .await
            .unwrap();
        let taken = remap.rebind("a", "go", Some("cmd+2".into())).await;
        assert!(matches!(taken, Err(CallError::Failed(m)) if m.contains("bound by b")));
        assert_eq!(remap.bound("a"), pairs(&[("go", "cmd+1")]));
        assert!(
            remap
                .rebind("a", "go", Some("hyper+x".into()))
                .await
                .is_err()
        );
        assert!(remap.rebind("a", "go", Some("fn".into())).await.is_err());
        assert!(
            remap
                .rebind("a", "nope", Some("cmd+3".into()))
                .await
                .is_err()
        );
        // Released, a plugin has nothing to rebind until it binds again.
        remap.release("a");
        assert!(remap.rebind("a", "go", Some("cmd+3".into())).await.is_err());
    }

    #[test]
    fn displays_sorted_modifiers() {
        let c = Chord::parse("shift+cmd+ctrl+k").unwrap();
        assert_eq!(c.to_string(), "ctrl+shift+cmd+KeyK");
    }
}
