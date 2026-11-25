//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.

/// How a line break in the clipboard is typed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Newline {
    /// A real Return, as `type-text` types it.
    Return,
    /// Shift+Return, which breaks the line without sending in Slack and Teams.
    ShiftReturn,
}

/// The settings from `init`, with defaults for missing keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub chord: String,
    pub newline: Newline,
    pub max_chars: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            chord: "cmd+shift+alt+t".into(),
            newline: Newline::Return,
            max_chars: 20_000,
        }
    }
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let d = Settings::default();
        let Ok(c) = serde_json::from_str::<serde_json::Value>(config) else {
            return d;
        };
        let chord = c["chord"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or(d.chord);
        let newline = match c["newline"].as_str() {
            Some("shift-return") => Newline::ShiftReturn,
            _ => Newline::Return,
        };
        let max_chars = c["max_chars"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(d.max_chars);
        Settings {
            chord,
            newline,
            max_chars,
        }
    }
}

/// What to do with the clipboard.
#[derive(Debug, PartialEq)]
pub enum Check {
    /// Nothing to type; the toast says why.
    Refuse(String),
    /// Type it; it holds this many characters.
    Type(usize),
}

pub fn check(text: Option<&str>, max_chars: usize) -> Check {
    let text = match text {
        Some(t) if !t.is_empty() => t,
        _ => return Check::Refuse("The clipboard holds no text".into()),
    };
    let count = text.chars().count();
    if count > max_chars {
        return Check::Refuse(format!(
            "The clipboard is {count} characters; the limit is {max_chars}"
        ));
    }
    Check::Type(count)
}

/// The text split at line breaks, with \r\n and \r taken as \n.
pub fn lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::to_string)
        .collect()
}

/// One call to the keyboard.
#[derive(Debug, PartialEq)]
pub enum Step {
    Type(String),
    ShiftEnter,
}

/// The keyboard calls that type `text`: one `type-text` for the whole of
/// it, or with shift-return, each non-empty line with shift+enter between.
pub fn steps(text: &str, newline: Newline) -> Vec<Step> {
    if newline == Newline::Return {
        return vec![Step::Type(text.to_string())];
    }
    let all = lines(text);
    let last = all.len().saturating_sub(1);
    let mut out = Vec::new();
    for (i, line) in all.into_iter().enumerate() {
        if !line.is_empty() {
            out.push(Step::Type(line));
        }
        if i < last {
            out.push(Step::ShiftEnter);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_when_missing_or_bad() {
        assert_eq!(Settings::from_json("{}"), Settings::default());
        assert_eq!(Settings::from_json("not json"), Settings::default());
        assert_eq!(
            Settings::from_json(r#"{"chord":" ","newline":"tab"}"#),
            Settings::default()
        );
    }

    #[test]
    fn settings_read_every_key() {
        let s = Settings::from_json(
            r#"{"chord":"ctrl+alt+t","newline":"shift-return","max_chars":50}"#,
        );
        assert_eq!(
            s,
            Settings {
                chord: "ctrl+alt+t".into(),
                newline: Newline::ShiftReturn,
                max_chars: 50,
            }
        );
    }

    #[test]
    fn check_refuses_empty_and_long() {
        assert_eq!(
            check(None, 10),
            Check::Refuse("The clipboard holds no text".into())
        );
        assert_eq!(
            check(Some(""), 10),
            Check::Refuse("The clipboard holds no text".into())
        );
        assert_eq!(
            check(Some("abcdef"), 5),
            Check::Refuse("The clipboard is 6 characters; the limit is 5".into())
        );
    }

    #[test]
    fn check_counts_characters_not_bytes() {
        assert_eq!(check(Some("héllo"), 5), Check::Type(5));
    }

    #[test]
    fn lines_split_every_kind_of_break() {
        assert_eq!(lines("a\r\nb\rc\nd"), vec!["a", "b", "c", "d"]);
        assert_eq!(lines("a\n"), vec!["a", ""]);
        assert_eq!(lines("one"), vec!["one"]);
    }

    #[test]
    fn return_types_the_whole_text_once() {
        assert_eq!(
            steps("a\nb", Newline::Return),
            vec![Step::Type("a\nb".into())]
        );
    }

    #[test]
    fn shift_return_breaks_between_lines_and_skips_empty_ones() {
        assert_eq!(
            steps("a\n\nb\n", Newline::ShiftReturn),
            vec![
                Step::Type("a".into()),
                Step::ShiftEnter,
                Step::ShiftEnter,
                Step::Type("b".into()),
                Step::ShiftEnter,
            ]
        );
    }
}
