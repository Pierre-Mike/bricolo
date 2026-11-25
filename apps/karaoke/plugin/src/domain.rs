//! The logic, with no capability calls and no generated bindings, so
//! `cargo test` runs it natively.
//!
//! The first half is the script's lib/karaoke.lua: what is playing comes
//! from `media-control get`, lyrics from LRCLIB (lrclib.net), a free
//! database of synced LRC lyrics with no key. It turns one into a lookup on
//! the other, reads the LRC, and works out which line is being sung right
//! now. The second half is the script's own state: the LRCLIB lookup, one
//! request per step, and what the caption and the tile show.

use std::collections::HashMap;

use serde_json::Value;

pub const API: &str = "https://lrclib.net/api";

/// How far a database entry's length may be from what is playing before it
/// is the wrong recording. A music video runs a few seconds longer than the
/// album cut; a live version or an extended mix runs a lot longer.
pub const MAX_DRIFT: f64 = 10.0;

const NOTE: &str = "♪";

/// The settings from `init`, with defaults for missing keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub chord: String,
    pub tick_ms: u32,
    /// Ticks between media-control reads.
    pub poll_every: u64,
    /// Seconds paused before the caption goes.
    pub pause_hide: f64,
    /// Seconds "no lyrics" stays up.
    pub none_hide: f64,
    pub get_tries: u32,
    /// Seconds per press of the timing buttons.
    pub nudge: f64,
}

impl Settings {
    pub fn from_json(config: &str) -> Self {
        let c = serde_json::from_str::<Value>(config).unwrap_or(Value::Null);
        let int = |key: &str, default: u64| c[key].as_u64().unwrap_or(default);
        let float = |key: &str, default: f64| c[key].as_f64().unwrap_or(default);
        Settings {
            chord: c["chord"].as_str().unwrap_or("cmd+alt+shift+k").to_string(),
            tick_ms: u32::try_from(int("tick_ms", 250)).unwrap_or(250).max(10),
            poll_every: int("poll_every", 4).max(1),
            pause_hide: float("pause_hide", 3.0),
            none_hide: float("none_hide", 5.0),
            get_tries: u32::try_from(int("get_tries", 4)).unwrap_or(4).clamp(1, 10),
            nudge: float("nudge", 0.5),
        }
    }
}

// ── Lua's patterns ─────────────────────────────────────────────────────────
// Lua's %s, %w and lower() know ASCII only, and every pattern below cuts at
// an ASCII byte, so slicing at those bytes keeps the UTF-8 whole.

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii() && is_space(c as u8))
}

fn trim_end(s: &str) -> &str {
    s.trim_end_matches(|c: char| c.is_ascii() && is_space(c as u8))
}

/// Runs of spaces to one space, then trimmed.
fn squash(s: &str) -> String {
    s.split(|c: char| c.is_ascii() && is_space(c as u8))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `word` with no letter or digit on either side, like `%f[%w]hd%f[%W]`.
fn has_word(s: &str, word: &str) -> bool {
    let b = s.as_bytes();
    let w = word.as_bytes();
    (0..=b.len().saturating_sub(w.len())).any(|i| {
        b[i..].starts_with(w)
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && b.get(i + w.len())
                .is_none_or(|c| !c.is_ascii_alphanumeric())
    })
}

/// `%s*([%(%[])([^%)%]]*)[%)%]]` at byte `i`: the end of the match and the
/// bracket's inside.
fn bracket_at(s: &str, i: usize) -> Option<(usize, &str)> {
    let b = s.as_bytes();
    let mut j = i;
    while j < b.len() && is_space(b[j]) {
        j += 1;
    }
    if j >= b.len() || !matches!(b[j], b'(' | b'[') {
        return None;
    }
    let close = j + 1 + b[j + 1..].iter().position(|c| matches!(c, b')' | b']'))?;
    Some((close + 1, &s[j + 1..close]))
}

/// Every bracket with the spaces before it gone where `drop(inside)` says
/// so, like the gsub calls of the Lua.
fn drop_brackets(s: &str, drop: impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut copied, mut i) = (0, 0);
    while i < s.len() {
        match bracket_at(s, i) {
            Some((end, inner)) => {
                out.push_str(&s[copied..i]);
                if !drop(inner) {
                    out.push_str(&s[i..end]);
                }
                copied = end;
                i = end;
            }
            None => i += 1,
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// `s` without a last word of `n` ASCII bytes that `is` accepts and the
/// spaces before it, like `%s+[Hh][DdQq]$`. Needs at least one space.
fn cut_last_word(s: &str, n: usize, is: impl Fn(&[u8]) -> bool) -> &str {
    let b = s.as_bytes();
    if b.len() > n && is(&b[b.len() - n..]) && is_space(b[b.len() - n - 1]) {
        trim_end(&s[..b.len() - n])
    } else {
        s
    }
}

// ── LRC ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Seconds into the song.
    pub t: f64,
    pub text: String,
}

/// `[offset:+500]` anywhere in the text, in seconds.
fn lrc_offset(text: &str) -> f64 {
    for (at, _) in text.match_indices("[offset:") {
        let b = text.as_bytes();
        let mut j = at + "[offset:".len();
        while j < b.len() && is_space(b[j]) {
            j += 1;
        }
        let from = j;
        if j < b.len() && matches!(b[j], b'+' | b'-') {
            j += 1;
        }
        let digits = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == digits {
            continue;
        }
        let number = &text[from..j];
        while j < b.len() && is_space(b[j]) {
            j += 1;
        }
        if b.get(j) == Some(&b']') {
            return number.parse::<f64>().unwrap_or(0.0) / 1000.0;
        }
    }
    0.0
}

/// One `[mm:ss]`, `[mm:ss.xx]` or `[mm:ss.xxx]` at the start of `s`, after
/// any spaces: its time and what follows it.
fn stamp(s: &str) -> Option<(f64, &str)> {
    let b = s.as_bytes();
    let digits = |from: usize| from + b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut j = 0;
    while j < b.len() && is_space(b[j]) {
        j += 1;
    }
    if b.get(j) != Some(&b'[') {
        return None;
    }
    let mm_end = digits(j + 1);
    if mm_end == j + 1 || b.get(mm_end) != Some(&b':') {
        return None;
    }
    let ss_end = digits(mm_end + 1);
    if ss_end == mm_end + 1 {
        return None;
    }
    let mut k = ss_end;
    if b.get(k) == Some(&b'.') {
        k += 1;
    }
    let frac_end = digits(k);
    if b.get(frac_end) != Some(&b']') {
        return None;
    }
    let num = |a: usize, z: usize| s[a..z].parse::<f64>().unwrap_or(0.0);
    let mut t = num(j + 1, mm_end) * 60.0 + num(mm_end + 1, ss_end);
    if frac_end > k {
        t += num(k, frac_end) / 10f64.powi((frac_end - k) as i32);
    }
    Some((t, &s[frac_end + 1..]))
}

/// A line can carry several stamps when a chorus repeats. `[ar:]`, `[ti:]`
/// and friends are skipped, and `[offset:+500]` moves every line half a
/// second sooner, which is what the LRC format means by a positive offset.
pub fn parse_lrc(text: &str) -> Vec<Line> {
    let offset = lrc_offset(text);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let mut rest = raw.strip_suffix('\r').unwrap_or(raw);
        let mut stamps = Vec::new();
        while let Some((t, after)) = stamp(rest) {
            stamps.push(t);
            rest = after;
        }
        for t in stamps {
            out.push(Line {
                t: (t - offset).max(0.0),
                text: trim(rest).to_string(),
            });
        }
    }
    // Stable for equal stamps, so two lines stamped alike keep file order.
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

/// How many lines have started: the 1-based index of the current line, or
/// 0 before the first.
pub fn line_at(lines: &[Line], pos: f64) -> usize {
    lines.partition_point(|l| l.t <= pos)
}

/// How far through line `i` (1-based) the sweep should be, 0 to 1. LRC only
/// says when a line starts, so its end is a guess: about 0.15 s a character,
/// never under two seconds, and never past the next line.
pub fn progress(lines: &[Line], i: usize, pos: f64) -> f64 {
    let Some(l) = i.checked_sub(1).and_then(|i| lines.get(i)) else {
        return 0.0;
    };
    let mut finish = l.t + (l.text.chars().count() as f64 * 0.15).max(2.0);
    if let Some(next) = lines.get(i) {
        finish = finish.min(next.t - 0.2);
    }
    if finish <= l.t {
        return 1.0;
    }
    ((pos - l.t) / (finish - l.t)).clamp(0.0, 1.0)
}

// ── Now playing ────────────────────────────────────────────────────────────

/// A YouTube tab's title is "Artist - Song (Official Video)" and its artist
/// is the channel, so a browser gets its title taken apart before the lookup.
pub const BROWSERS: &[&str] = &[
    "com.apple.Safari",
    "com.apple.SafariTechnologyPreview",
    "com.apple.WebKit.GPU",
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "company.thebrowser.Browser",
    "company.thebrowser.dia",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "org.mozilla.firefox",
    "app.zen-browser.zen",
    "com.vivaldi.Vivaldi",
    "com.operasoftware.Opera",
    "com.kagi.kagimacOS",
];

#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub app: String,
    pub browser: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub rate: f64,
    pub duration: Option<f64>,
    pub elapsed: f64,
    /// The epoch second at which `elapsed` was true.
    pub at: Option<f64>,
    /// App, artist and title, one per line: what the caches are keyed on.
    pub key: String,
}

/// Lua's `tonumber`: a number, or a string that holds one.
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| trim(s).parse().ok()))
}

/// One `media-control get --micros` payload into seconds, or None when
/// nothing is playing.
pub fn normalize(raw: &Value) -> Option<NowPlaying> {
    let title = raw["title"].as_str().filter(|t| !t.is_empty())?;
    let app = raw["parentApplicationBundleIdentifier"]
        .as_str()
        .or(raw["bundleIdentifier"].as_str())
        .unwrap_or("?");
    let playing = raw["playing"] == Value::Bool(true);
    let micros = |k: &str| number(&raw[k]).map(|m| m / 1e6);
    let artist = raw["artist"].as_str().unwrap_or("");
    Some(NowPlaying {
        app: app.into(),
        browser: BROWSERS.contains(&app),
        title: title.into(),
        artist: artist.into(),
        album: raw["album"].as_str().unwrap_or("").into(),
        playing,
        rate: number(&raw["playbackRate"]).unwrap_or(if playing { 1.0 } else { 0.0 }),
        duration: micros("durationMicros").or_else(|| number(&raw["duration"])),
        elapsed: micros("elapsedTimeMicros")
            .or_else(|| number(&raw["elapsedTime"]))
            .unwrap_or(0.0),
        at: micros("timestampEpochMicros").or_else(|| number(&raw["timestamp"])),
        key: [app, artist, title].join("\n"),
    })
}

/// Seconds into the song at epoch second `now`.
pub fn position(np: &NowPlaying, now: f64) -> f64 {
    let mut pos = np.elapsed;
    if let (true, Some(at)) = (np.playing, np.at) {
        pos += (now - at) * np.rate;
    }
    if let Some(d) = np.duration {
        pos = pos.min(d);
    }
    pos.max(0.0)
}

// ── Cleaning titles ────────────────────────────────────────────────────────

/// A bracket is noise when it describes the upload rather than the song.
const NOISE: &[&str] = &[
    "official",
    "video",
    "audio",
    "lyric",
    "visuali",
    "paroles",
    "clip",
    "prod.",
    "prod ",
    "produced",
    "explicit",
    "color coded",
    "m/v",
];
/// Noise only as whole words.
const NOISE_WORDS: &[&str] = &["hd", "hq", "4k", "mv"];

fn is_noise(inner: &str) -> bool {
    let s = inner.to_ascii_lowercase();
    NOISE.iter().any(|w| s.contains(w)) || NOISE_WORDS.iter().any(|w| has_word(&s, w))
}

pub fn clean_title(s: &str) -> String {
    let s = match s.find('|') {
        Some(i) => trim_end(&s[..i]),
        None => s,
    };
    let s = drop_brackets(s, is_noise);
    let s = cut_last_word(&s, 2, |w| {
        matches!(w[0], b'H' | b'h') && matches!(w[1], b'D' | b'd' | b'Q' | b'q')
    });
    let s = cut_last_word(s, 2, |w| w[0] == b'4' && matches!(w[1], b'K' | b'k'));
    squash(s)
}

pub fn clean_artist(s: &str) -> String {
    let mut s = s;
    if let Some(rest) = s.strip_suffix("Topic")
        && let Some(rest) = trim_end(rest).strip_suffix('-')
    {
        s = trim_end(rest);
    }
    let s = s.strip_suffix("VEVO").unwrap_or(s);
    let s = cut_last_word(s, 8, |w| {
        matches!(w[0], b'O' | b'o') && &w[1..] == b"fficial"
    });
    squash(s)
}

/// Spotify writes the edition after a dash: "Song - Remastered 2011".
const EDITION: &[&str] = &[
    "remaster", "edit", "version", "live", "mono", "stereo", "mix",
];

const DASHES: &[&str] = &[" - ", " – ", " — "];

/// The first "a - b" split on any dash.
fn split_dash(s: &str) -> Option<(&str, &str)> {
    DASHES
        .iter()
        .filter_map(|d| s.find(d).map(|i| (i, d.len())))
        .min_by_key(|(i, _)| *i)
        .map(|(i, len)| (&s[..i], &s[i + len..]))
}

/// Where the last dash starts, for the edition suffix.
fn last_dash(s: &str) -> Option<usize> {
    DASHES.iter().filter_map(|d| s.rfind(d)).max()
}

/// Where `%s+<word><dot>%s` starts in `lower`, with the dot optional or
/// required.
fn credit_at(lower: &str, word: &str, dot_required: bool) -> Option<usize> {
    let b = lower.as_bytes();
    lower.match_indices(word).find_map(|(i, _)| {
        if i == 0 || !is_space(b[i - 1]) {
            return None;
        }
        let mut j = i + word.len();
        if b.get(j) == Some(&b'.') {
            j += 1;
        } else if dot_required {
            return None;
        }
        if !b.get(j).is_some_and(|c| is_space(*c)) {
            return None;
        }
        Some(trim_end(&lower[..i]).len())
    })
}

/// The title with every bracket, edition and feature credit gone: the name a
/// lyrics database is most likely to file the song under.
pub fn bare_title(s: &str) -> String {
    let mut s = drop_brackets(&clean_title(s), |_| true);
    if let Some(i) = last_dash(&s) {
        let tail = s[i..].to_ascii_lowercase();
        if EDITION.iter().any(|w| tail.contains(w)) {
            s.truncate(i);
        }
    }
    let lower = s.to_ascii_lowercase();
    if let Some(f) = credit_at(&lower, "feat", false).or_else(|| credit_at(&lower, "ft", true)) {
        s.truncate(f);
    }
    squash(&s)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub artist: String,
    pub title: String,
}

/// Every artist and title worth asking LRCLIB for, best guess first.
pub fn candidates(np: &NowPlaying) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut add = |artist: &str, title: &str| {
        let (artist, title) = (trim(artist), trim(title));
        if artist.is_empty() || title.is_empty() {
            return;
        }
        let seen = out
            .iter()
            .any(|c| c.artist.eq_ignore_ascii_case(artist) && c.title.eq_ignore_ascii_case(title));
        if !seen {
            out.push(Candidate {
                artist: artist.into(),
                title: title.into(),
            });
        }
    };
    if np.browser {
        if let Some((a, t)) = split_dash(&np.title) {
            add(&clean_artist(a), &clean_title(t));
            add(&clean_artist(a), &bare_title(t));
        }
        add(&clean_artist(&np.artist), &clean_title(&np.title));
        add(&clean_artist(&np.artist), &bare_title(&np.title));
        // LRCLIB also holds entries filed under the raw YouTube title.
        add(&np.artist, &np.title);
    } else {
        add(&np.artist, &np.title);
        add(&np.artist, &bare_title(&np.title));
    }
    out
}

// ── LRCLIB ─────────────────────────────────────────────────────────────────

/// /get matches exactly on artist and title and within two seconds on
/// length, which is why the duration is sent when it is known.
pub fn get_url(c: &Candidate, duration: Option<f64>) -> String {
    let mut u = format!(
        "{API}/get?artist_name={}&track_name={}",
        urlencode(&c.artist),
        urlencode(&c.title)
    );
    if let Some(d) = duration {
        u.push_str(&format!("&duration={}", (d + 0.5).floor() as i64));
    }
    u
}

pub fn search_url(q: &str) -> String {
    format!("{API}/search?q={}", urlencode(q))
}

/// /search on the fields is fuzzy, so "Starry Night" by Ren finds "Vincent's
/// Tale - Starry Night - Act I", and it answers with every length on file.
pub fn search_fields_url(c: &Candidate) -> String {
    format!(
        "{API}/search?track_name={}&artist_name={}",
        urlencode(&c.title),
        urlencode(&c.artist)
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StepKind {
    Get,
    Search,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    pub url: String,
}

/// Every request worth making for one song, in order. The exact match on
/// the best guess comes first because it is one answer and usually right;
/// the field search next, because it forgives a title that is close; then
/// the other guesses; then free text as the last resort.
pub fn plan(np: &NowPlaying) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::new();
    let mut add = |kind, url: String| {
        if !out.iter().any(|s| s.url == url) {
            out.push(Step { kind, url });
        }
    };
    let tries = candidates(np);
    let Some(first) = tries.first() else {
        return out;
    };
    let bare = bare_title(&first.title);
    add(StepKind::Get, get_url(first, np.duration));
    add(
        StepKind::Search,
        search_fields_url(&Candidate {
            artist: first.artist.clone(),
            title: bare.clone(),
        }),
    );
    for c in &tries[1..] {
        add(StepKind::Get, get_url(c, np.duration));
    }
    add(
        StepKind::Search,
        search_url(&format!("{} {bare}", first.artist)),
    );
    out
}

/// From /search results: the synced entry closest in length, if any is close
/// enough to be the same recording.
pub fn pick_best(results: Option<&Value>, duration: Option<f64>) -> Option<&Value> {
    let mut best: Option<(&Value, f64)> = None;
    let list = results.and_then(Value::as_array)?;
    for r in list.iter().take_while(|r| !r.is_null()) {
        if !r["syncedLyrics"].as_str().is_some_and(|l| !l.is_empty()) {
            continue;
        }
        let Some(duration) = duration else {
            return Some(r);
        };
        let diff = (number(&r["duration"]).unwrap_or(0.0) - duration).abs();
        if diff <= MAX_DRIFT && best.is_none_or(|(_, d)| diff < d) {
            best = Some((r, diff));
        }
    }
    best.map(|(r, _)| r)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lyrics {
    Loading,
    Synced(Vec<Line>),
    /// LRCLIB has no synced lyrics for the song.
    NoneFound,
    Instrumental,
    /// LRCLIB did not answer.
    Error,
}

/// One LRCLIB record into lyrics.
pub fn from_record(rec: Option<&Value>) -> Lyrics {
    let Some(rec) = rec.filter(|r| r.is_object() || r.is_array()) else {
        return Lyrics::NoneFound;
    };
    if rec["instrumental"] == Value::Bool(true) {
        return Lyrics::Instrumental;
    }
    let lines = parse_lrc(rec["syncedLyrics"].as_str().unwrap_or(""));
    if lines.is_empty() {
        Lyrics::NoneFound
    } else {
        Lyrics::Synced(lines)
    }
}

// ── What the caption shows ─────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub struct Display {
    pub current: String,
    pub next: String,
    pub progress: f64,
}

fn name(np: &NowPlaying) -> String {
    if np.artist.is_empty() {
        np.title.clone()
    } else {
        format!("{} · {}", np.title, np.artist)
    }
}

pub fn display(np: &NowPlaying, lyrics: Option<&Lyrics>, pos: f64) -> Display {
    let show = |current: String, next: String| Display {
        current,
        next,
        progress: 0.0,
    };
    let lines = match lyrics.unwrap_or(&Lyrics::Loading) {
        Lyrics::Loading => return show(name(np), "Looking up lyrics…".into()),
        Lyrics::NoneFound => return show(name(np), "No synced lyrics for this one".into()),
        Lyrics::Instrumental => return show(format!("{NOTE} Instrumental"), name(np)),
        Lyrics::Error => return show(name(np), "LRCLIB is busy, trying again soon".into()),
        Lyrics::Synced(lines) => lines,
    };
    let i = line_at(lines, pos);
    // lines[i] is the line after the current one, which is lines[i - 1].
    let next = lines.get(i).map(|l| l.text.clone()).unwrap_or_default();
    if i == 0 {
        return show(NOTE.into(), next);
    }
    let text = &lines[i - 1].text;
    if text.is_empty() {
        // The next sung line, not the next stamp, which may be another gap.
        let sung = lines[i..].iter().find(|l| !l.text.is_empty());
        return show(
            NOTE.into(),
            sung.map(|l| l.text.clone()).unwrap_or_default(),
        );
    }
    Display {
        current: text.clone(),
        next,
        progress: progress(lines, i, pos),
    }
}

pub fn status(np: Option<&NowPlaying>, lyrics: Option<&Lyrics>) -> String {
    if np.is_none() {
        return "Nothing playing".into();
    }
    match lyrics.unwrap_or(&Lyrics::Loading) {
        Lyrics::Loading => "Looking up lyrics…".into(),
        Lyrics::NoneFound => "No synced lyrics found".into(),
        Lyrics::Instrumental => "Instrumental".into(),
        Lyrics::Error => "LRCLIB did not answer".into(),
        Lyrics::Synced(lines) => format!("Synced · {} lines", lines.len()),
    }
}

/// A key as one line, for the log.
pub fn one_line(key: &str) -> String {
    key.replace('\n', " / ")
}

// ── The lookup ─────────────────────────────────────────────────────────────

/// What the lookup wants next.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// A GET of this URL; its answer goes to `Lookup::answer`.
    Request(String),
    /// LRCLIB is busy: wait this many milliseconds, then `Lookup::resume`.
    Wait(u32),
    /// Lyrics, or the word that it is instrumental.
    Found(Lyrics),
    /// Every step failed: `NoneFound`, or `Error` when LRCLIB did not answer.
    GaveUp(Lyrics),
    /// Something else is playing now.
    Stop,
}

/// The LRCLIB lookup for one song: the steps of `plan`, each tried until
/// LRCLIB answers something other than 429 or 503, waiting 1, 2, 4… s
/// between tries.
#[derive(Debug)]
pub struct Lookup {
    pub song: NowPlaying,
    steps: Vec<Step>,
    step: usize,
    /// Tries made on this step.
    tries: u32,
    max_tries: u32,
    /// A step answered something other than 200 or 404.
    failed: bool,
}

impl Lookup {
    pub fn new(song: NowPlaying, max_tries: u32) -> Self {
        Lookup {
            steps: plan(&song),
            song,
            step: 0,
            tries: 0,
            max_tries: max_tries.max(1),
            failed: false,
        }
    }

    /// Starts or retries a step. `playing` is the key of what is playing
    /// now; a new step only starts while it is still this song.
    pub fn resume(&mut self, playing: Option<&str>) -> Next {
        if self.tries == 0 {
            if self.step >= self.steps.len() {
                return Next::GaveUp(if self.failed {
                    Lyrics::Error
                } else {
                    Lyrics::NoneFound
                });
            }
            if playing != Some(self.song.key.as_str()) {
                return Next::Stop;
            }
        } else if self.tries >= self.max_tries {
            // Out of tries: the step got no answer.
            return self.settle(0, None, playing);
        }
        self.tries += 1;
        Next::Request(self.steps[self.step].url.clone())
    }

    /// The answer to the last `Request`. `status` is 0 when the request
    /// failed outright.
    pub fn answer(&mut self, status: u16, body: &str, playing: Option<&str>) -> Next {
        if matches!(status, 0 | 429 | 503) {
            let wait = 1000u32.saturating_mul(1 << (self.tries - 1).min(20));
            return Next::Wait(wait);
        }
        self.settle(status, serde_json::from_str(body).ok(), playing)
    }

    fn settle(&mut self, status: u16, body: Option<Value>, playing: Option<&str>) -> Next {
        if status != 200 && status != 404 {
            self.failed = true;
        }
        let rec = body.filter(|_| status == 200);
        let found = match self.steps[self.step].kind {
            StepKind::Get => from_record(rec.as_ref()),
            StepKind::Search => from_record(pick_best(rec.as_ref(), self.song.duration)),
        };
        if found != Lyrics::NoneFound {
            return Next::Found(found);
        }
        self.step += 1;
        self.tries = 0;
        self.resume(playing)
    }
}

// ── The script's state ─────────────────────────────────────────────────────

/// What is playing and its lyrics, and the caches that survive songs.
#[derive(Debug, Default)]
pub struct Karaoke {
    pub np: Option<NowPlaying>,
    pub lyrics: Option<Lyrics>,
    cache: HashMap<String, Lyrics>,
    offsets: HashMap<String, f64>,
    /// When the song paused, and when "none" came back.
    since_pause: Option<f64>,
    since_none: Option<f64>,
}

/// A new song from `on_playing`.
#[derive(Debug, PartialEq)]
pub struct NewSong {
    /// Its lyrics are not in the cache.
    pub needs_lookup: bool,
}

impl Karaoke {
    pub fn key(&self) -> Option<&str> {
        self.np.as_ref().map(|n| n.key.as_str())
    }

    fn offset(&self) -> f64 {
        self.key()
            .and_then(|k| self.offsets.get(k))
            .copied()
            .unwrap_or(0.0)
    }

    /// What `media-control` says is playing, read at epoch second `now`.
    pub fn on_playing(&mut self, next: Option<NowPlaying>, now: f64) -> Option<NewSong> {
        let was = self.np.take().map(|n| n.key);
        self.np = next;
        let Some(np) = &self.np else {
            self.lyrics = None;
            return None;
        };
        if np.playing {
            self.since_pause = None;
        } else if self.since_pause.is_none() {
            self.since_pause = Some(now);
        }
        if was.as_deref() == Some(np.key.as_str()) {
            return None;
        }
        let cached = self.cache.get(&np.key).cloned();
        let needs_lookup = cached.is_none();
        let lyrics = cached.unwrap_or(Lyrics::Loading);
        self.since_none = (lyrics == Lyrics::NoneFound).then_some(now);
        self.lyrics = Some(lyrics);
        Some(NewSong { needs_lookup })
    }

    /// Lyrics a lookup found for `key`.
    pub fn found(&mut self, key: &str, lyrics: Lyrics) {
        self.cache.insert(key.into(), lyrics.clone());
        if self.key() == Some(key) {
            self.lyrics = Some(lyrics);
        }
    }

    /// A lookup for `key` that found nothing, at epoch second `now`. An
    /// error is not cached, so the next play of the song asks again.
    pub fn gave_up(&mut self, key: &str, lyrics: Lyrics, now: f64) {
        if lyrics == Lyrics::NoneFound {
            self.cache.insert(key.into(), lyrics.clone());
        }
        if self.key() == Some(key) {
            self.lyrics = Some(lyrics);
            self.since_none = Some(now);
        }
    }

    /// Moves the lyrics of what is playing by `delta` seconds. False when
    /// nothing plays.
    pub fn nudge(&mut self, delta: f64) -> bool {
        let Some(key) = self.key().map(String::from) else {
            return false;
        };
        *self.offsets.entry(key).or_insert(0.0) += delta;
        true
    }

    fn wanted(&self, t: f64, enabled: bool, s: &Settings) -> bool {
        let Some(np) = self.np.as_ref().filter(|_| enabled) else {
            return false;
        };
        if !np.playing && t - self.since_pause.unwrap_or(t) >= s.pause_hide {
            return false;
        }
        !self.since_none.is_some_and(|n| t - n >= s.none_hide)
    }

    /// The caption's text at epoch second `t`, or None to hide it.
    pub fn caption(&self, t: f64, enabled: bool, s: &Settings) -> Option<String> {
        if !self.wanted(t, enabled, s) {
            return None;
        }
        let np = self.np.as_ref()?;
        let pos = position(np, t) + self.offset();
        let d = display(np, self.lyrics.as_ref(), pos);
        Some(if d.next.is_empty() {
            d.current
        } else {
            format!("{}\n{}", d.current, d.next)
        })
    }

    /// The tile's status line.
    pub fn tile_status(&self, enabled: bool) -> String {
        if !enabled {
            return "Off".into();
        }
        let status = status(self.np.as_ref(), self.lyrics.as_ref());
        let Some(np) = &self.np else {
            return status;
        };
        let mut line = format!("{} · {status}", name(np));
        let off = self.offset();
        if off != 0.0 {
            line.push_str(&format!(" · {off:+.1} s"));
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chrome_video() -> NowPlaying {
        normalize(&json!({
            "title": "Daft Punk - One More Time (Official Video)",
            "artist": "DaftPunkVEVO",
            "bundleIdentifier": "com.google.Chrome",
            "playing": true,
            "durationMicros": 320_500_000,
            "elapsedTimeMicros": 10_000_000,
            "timestampEpochMicros": 1_700_000_000_000_000i64,
        }))
        .unwrap()
    }

    fn spotify(title: &str, playing: bool) -> NowPlaying {
        normalize(&json!({
            "title": title,
            "artist": "The Beatles",
            "bundleIdentifier": "com.spotify.client",
            "playing": playing,
            "durationMicros": 431_000_000,
            "elapsedTimeMicros": 5_000_000,
            "timestampEpochMicros": 1_000_000_000,
        }))
        .unwrap()
    }

    fn lines(spec: &[(f64, &str)]) -> Vec<Line> {
        spec.iter()
            .map(|(t, text)| Line {
                t: *t,
                text: text.to_string(),
            })
            .collect()
    }

    #[test]
    fn parses_lrc_with_offset_repeats_and_tags() {
        let got = parse_lrc(
            "[ar:X]\n[offset:+500]\n[00:01.00]one\r\n[00:03.5][00:10.123] chorus \n[00:02]two\n[00:05.00]\n",
        );
        let want = lines(&[
            (0.5, "one"),
            (1.5, "two"),
            (3.0, "chorus"),
            (4.5, ""),
            (9.623, "chorus"),
        ]);
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            assert!((g.t - w.t).abs() < 1e-9, "{g:?} {w:?}");
            assert_eq!(g.text, w.text);
        }
        assert!(parse_lrc("").is_empty());
        assert_eq!(parse_lrc("[offset:-1000]\n[00:01]a")[0].t, 2.0);
        assert_eq!(parse_lrc("[01:00]b\n[01:00]a")[0].text, "b");
    }

    #[test]
    fn finds_the_line_and_its_progress() {
        let l = parse_lrc("[00:01.00]one\n[00:03.5] chorus\n[00:02]two\n[00:05.00]\n");
        assert_eq!(line_at(&l, 0.5), 0);
        assert_eq!(line_at(&l, 1.0), 1);
        assert_eq!(line_at(&l, 99.0), 4);
        let l = parse_lrc("[00:00.5]one\n[00:01.5]two\n[00:03]chorus");
        assert!((progress(&l, 1, 1.0) - 0.625).abs() < 1e-9);
        assert!((progress(&l, 2, 2.0) - 0.384_615_384_615_384_7).abs() < 1e-9);
        assert_eq!(progress(&l, 0, 2.0), 0.0);
    }

    #[test]
    fn normalizes_a_payload() {
        let np = spotify("Hey Jude - Remastered 2015", false);
        assert_eq!(
            np.key,
            "com.spotify.client\nThe Beatles\nHey Jude - Remastered 2015"
        );
        assert_eq!(np.rate, 0.0);
        assert_eq!(np.duration, Some(431.0));
        assert!(!np.browser);
        assert!(chrome_video().browser);
        let np = normalize(
            &json!({"title": "T", "parentApplicationBundleIdentifier": "p",
            "bundleIdentifier": "b", "duration": "200", "elapsedTime": 3}),
        )
        .unwrap();
        assert_eq!(
            (np.app.as_str(), np.duration, np.elapsed),
            ("p", Some(200.0), 3.0)
        );
        assert_eq!(normalize(&json!({"title": ""})), None);
        assert_eq!(normalize(&Value::Null), None);
    }

    #[test]
    fn works_out_the_position() {
        let np = chrome_video();
        assert!((position(&np, 1_700_000_005.0) - 15.0).abs() < 1e-9);
        assert_eq!(position(&np, 1_800_000_000.0), 320.5);
        assert_eq!(position(&spotify("x", false), 9e9), 5.0);
    }

    #[test]
    fn cleans_titles_like_the_lua() {
        for (raw, want) in [
            (
                "Daft Punk - One More Time (Official Video) [HD]",
                "Daft Punk - One More Time",
            ),
            (
                "Song (feat. X) (Official Music Video) | Live at Wembley",
                "Song (feat. X)",
            ),
            ("Hello (Lyric Video) HD", "Hello"),
            ("Track [Prod. by Y]  4K", "Track"),
            ("Real Love (Remix) (official audio)", "Real Love (Remix)"),
            ("Song (4k remaster)", "Song"),
            ("Song (Live)", "Song (Live)"),
            ("Song (hdtv)", "Song (hdtv)"),
        ] {
            assert_eq!(clean_title(raw), want, "{raw}");
        }
        for (raw, want) in [
            ("Song - Remastered 2011", "Song"),
            ("Song (Live) - Single Version", "Song"),
            ("Hey Jude - 2015 Mix", "Hey Jude"),
            ("A - B", "A - B"),
            ("Song feat. Someone", "Song"),
            ("Song ft. Someone", "Song"),
            ("Song featuring X", "Song featuring X"),
            ("Lose Yourself – Live", "Lose Yourself"),
            ("Song (feat. X) [Live]", "Song"),
        ] {
            assert_eq!(bare_title(raw), want, "{raw}");
        }
        for (raw, want) in [
            ("Adele - Topic", "Adele"),
            ("AdeleVEVO", "Adele"),
            ("Adele Official", "Adele"),
            ("Adele-Topic", "Adele"),
            ("  The   Band  ", "The Band"),
        ] {
            assert_eq!(clean_artist(raw), want, "{raw}");
        }
    }

    #[test]
    fn plans_the_lookup_like_the_lua() {
        let pairs: Vec<_> = candidates(&chrome_video())
            .into_iter()
            .map(|c| (c.artist, c.title))
            .collect();
        assert_eq!(
            pairs,
            [
                ("Daft Punk".into(), "One More Time".into()),
                ("DaftPunk".into(), "Daft Punk - One More Time".into()),
                (
                    "DaftPunkVEVO".into(),
                    "Daft Punk - One More Time (Official Video)".into()
                ),
            ] as [(String, String); 3]
        );
        let urls: Vec<_> = plan(&chrome_video()).into_iter().map(|s| s.url).collect();
        assert_eq!(
            urls,
            [
                "https://lrclib.net/api/get?artist_name=Daft%20Punk&track_name=One%20More%20Time&duration=321",
                "https://lrclib.net/api/search?track_name=One%20More%20Time&artist_name=Daft%20Punk",
                "https://lrclib.net/api/get?artist_name=DaftPunk&track_name=Daft%20Punk%20-%20One%20More%20Time&duration=321",
                "https://lrclib.net/api/get?artist_name=DaftPunkVEVO&track_name=Daft%20Punk%20-%20One%20More%20Time%20%28Official%20Video%29&duration=321",
                "https://lrclib.net/api/search?q=Daft%20Punk%20One%20More%20Time",
            ]
        );
        let steps = plan(&spotify("Hey Jude - Remastered 2015", true));
        let kinds: Vec<_> = steps.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                StepKind::Get,
                StepKind::Search,
                StepKind::Get,
                StepKind::Search
            ]
        );
        assert_eq!(
            steps[2].url,
            "https://lrclib.net/api/get?artist_name=The%20Beatles&track_name=Hey%20Jude&duration=431"
        );
    }

    #[test]
    fn picks_the_closest_synced_result() {
        let results = json!([
            {"id": 1, "duration": 200, "syncedLyrics": ""},
            {"id": 2, "duration": 208, "syncedLyrics": "[00:01]a"},
            {"id": 3, "duration": 203, "syncedLyrics": "[00:01]a"},
            {"id": 4, "duration": 300, "syncedLyrics": "[00:01]a"},
        ]);
        assert_eq!(pick_best(Some(&results), Some(201.0)).unwrap()["id"], 3);
        assert_eq!(pick_best(Some(&results), None).unwrap()["id"], 2);
        assert_eq!(pick_best(Some(&results), Some(100.0)), None);
        assert_eq!(pick_best(Some(&json!({"a": 1})), None), None);
    }

    #[test]
    fn reads_a_record() {
        assert_eq!(from_record(None), Lyrics::NoneFound);
        assert_eq!(
            from_record(Some(&json!({"instrumental": true}))),
            Lyrics::Instrumental
        );
        assert_eq!(
            from_record(Some(&json!({"syncedLyrics": null}))),
            Lyrics::NoneFound
        );
        assert_eq!(
            from_record(Some(&json!({"syncedLyrics": "[00:01]hi"}))),
            Lyrics::Synced(lines(&[(1.0, "hi")]))
        );
    }

    #[test]
    fn displays_the_current_and_next_line() {
        let np = spotify("Hey Jude", true);
        let synced = Lyrics::Synced(lines(&[
            (1.0, "Hey Jude"),
            (4.0, ""),
            (6.0, ""),
            (8.0, "Take a sad song"),
        ]));
        let d = |pos| {
            let d = display(&np, Some(&synced), pos);
            (d.current, d.next)
        };
        assert_eq!(d(0.0), ("♪".into(), "Hey Jude".into()));
        assert_eq!(d(1.5), ("Hey Jude".into(), "".into()));
        assert_eq!(d(4.5), ("♪".into(), "Take a sad song".into()));
        assert_eq!(d(9.0), ("Take a sad song".into(), "".into()));
        let loading = display(&np, None, 0.0);
        assert_eq!(
            (loading.current.as_str(), loading.next.as_str()),
            ("Hey Jude · The Beatles", "Looking up lyrics…")
        );
        assert_eq!(
            display(&np, Some(&Lyrics::Instrumental), 0.0).current,
            "♪ Instrumental"
        );
        assert_eq!(status(Some(&np), Some(&synced)), "Synced · 4 lines");
        assert_eq!(status(None, Some(&synced)), "Nothing playing");
        assert_eq!(
            status(Some(&np), Some(&Lyrics::Error)),
            "LRCLIB did not answer"
        );
    }

    #[test]
    fn lookup_retries_busy_answers_then_moves_on() {
        let song = spotify("Hey Jude - Remastered 2015", true);
        let key = song.key.clone();
        let playing = Some(key.as_str());
        let mut l = Lookup::new(song, 3);
        let first = l.resume(playing);
        assert!(matches!(first, Next::Request(ref u) if u.contains("/get?")));
        assert_eq!(l.answer(503, "", playing), Next::Wait(1000));
        assert!(matches!(l.resume(playing), Next::Request(_)));
        assert_eq!(l.answer(0, "", playing), Next::Wait(2000));
        assert!(matches!(l.resume(playing), Next::Request(_)));
        assert_eq!(l.answer(429, "", playing), Next::Wait(4000));
        // Out of tries: the search comes next.
        assert!(
            matches!(l.resume(playing), Next::Request(ref u) if u.contains("/search?track_name"))
        );
        let found = l.answer(
            200,
            r#"[{"duration": 430, "syncedLyrics": "[00:01]Hey Jude"}]"#,
            playing,
        );
        assert_eq!(
            found,
            Next::Found(Lyrics::Synced(lines(&[(1.0, "Hey Jude")])))
        );
    }

    #[test]
    fn lookup_gives_up_or_stops() {
        let song = spotify("Hey Jude", true);
        let key = song.key.clone();
        let playing = Some(key.as_str());
        let mut l = Lookup::new(song.clone(), 4);
        let mut next = l.resume(playing);
        while let Next::Request(_) = next {
            next = l.answer(404, r#"{"message": "not found"}"#, playing);
        }
        assert_eq!(next, Next::GaveUp(Lyrics::NoneFound));

        let mut l = Lookup::new(song.clone(), 4);
        let mut next = l.resume(playing);
        while let Next::Request(_) = next {
            next = l.answer(500, "", playing);
        }
        assert_eq!(next, Next::GaveUp(Lyrics::Error));

        let mut l = Lookup::new(song, 4);
        assert!(matches!(l.resume(playing), Next::Request(_)));
        assert_eq!(l.answer(404, "", Some("something else")), Next::Stop);
        assert_eq!(Lookup::new(spotify("x", true), 4).resume(None), Next::Stop);
    }

    #[test]
    fn follows_songs_and_hides_the_caption() {
        let s = Settings::from_json("{}");
        let mut k = Karaoke::default();
        let np = spotify("Hey Jude", true);
        let key = np.key.clone();
        assert_eq!(
            k.on_playing(Some(np.clone()), 100.0),
            Some(NewSong { needs_lookup: true })
        );
        assert_eq!(k.on_playing(Some(np.clone()), 101.0), None);
        assert_eq!(
            k.tile_status(true),
            "Hey Jude · The Beatles · Looking up lyrics…"
        );
        assert_eq!(k.tile_status(false), "Off");
        assert_eq!(k.caption(101.0, false, &s), None);
        assert_eq!(
            k.caption(101.0, true, &s).unwrap(),
            "Hey Jude · The Beatles\nLooking up lyrics…"
        );

        k.found(
            &key,
            Lyrics::Synced(lines(&[(1.0, "Hey Jude"), (999.0, "na na")])),
        );
        assert_eq!(k.caption(1000.0, true, &s).unwrap(), "Hey Jude\nna na");
        assert!(k.nudge(0.5));
        assert_eq!(
            k.tile_status(true),
            "Hey Jude · The Beatles · Synced · 2 lines · +0.5 s"
        );

        // Paused: the caption stays for pause_hide seconds.
        let paused = spotify("Hey Jude", false);
        assert_eq!(k.on_playing(Some(paused.clone()), 200.0), None);
        assert!(k.caption(202.0, true, &s).is_some());
        assert_eq!(k.caption(203.0, true, &s), None);

        // Back to it later: the lyrics come from the cache.
        assert_eq!(k.on_playing(None, 300.0), None);
        assert_eq!(k.tile_status(true), "Nothing playing");
        assert!(!k.nudge(0.5));
        assert_eq!(
            k.on_playing(Some(np), 301.0),
            Some(NewSong {
                needs_lookup: false
            })
        );
        assert_eq!(
            k.tile_status(true),
            "Hey Jude · The Beatles · Synced · 2 lines · +0.5 s"
        );
    }

    #[test]
    fn none_found_shows_for_a_while_and_is_cached() {
        let s = Settings::from_json("{}");
        let mut k = Karaoke::default();
        let np = spotify("Obscure", true);
        let key = np.key.clone();
        k.on_playing(Some(np.clone()), 10.0);
        k.gave_up(&key, Lyrics::NoneFound, 10.0);
        assert!(
            k.caption(14.0, true, &s)
                .unwrap()
                .ends_with("No synced lyrics for this one")
        );
        assert_eq!(k.caption(15.0, true, &s), None);
        k.on_playing(None, 20.0);
        assert_eq!(
            k.on_playing(Some(np), 21.0),
            Some(NewSong {
                needs_lookup: false
            })
        );
        assert!(k.caption(25.0, true, &s).is_some());
        assert_eq!(k.caption(26.0, true, &s), None);

        // An error is not cached, and a late answer for another song
        // leaves what is playing alone.
        let mut k = Karaoke::default();
        k.on_playing(Some(spotify("A", true)), 0.0);
        k.gave_up("elsewhere", Lyrics::Error, 1.0);
        assert_eq!(k.lyrics, Some(Lyrics::Loading));
    }

    #[test]
    fn settings_fall_back_to_defaults() {
        let s = Settings::from_json("{}");
        assert_eq!(s.chord, "cmd+alt+shift+k");
        assert_eq!((s.tick_ms, s.poll_every, s.get_tries), (250, 4, 4));
        assert_eq!((s.pause_hide, s.none_hide, s.nudge), (3.0, 5.0, 0.5));
        let s = Settings::from_json(r#"{"chord":"f19","tick_ms":1,"poll_every":0,"nudge":0.25}"#);
        assert_eq!(
            (s.chord.as_str(), s.tick_ms, s.poll_every, s.nudge),
            ("f19", 10, 1, 0.25)
        );
        assert_eq!(Settings::from_json("junk"), Settings::from_json("{}"));
    }

    #[test]
    fn encodes_like_the_lua() {
        assert_eq!(urlencode("a b-_.~é/"), "a%20b-_.~%C3%A9%2F");
        assert_eq!(one_line("a\nb\nc"), "a / b / c");
    }
}
