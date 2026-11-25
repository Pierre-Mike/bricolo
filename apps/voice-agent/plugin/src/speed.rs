//! The speed widget: where the wait between "you stop talking" and "the
//! agent starts talking" goes, read from the daemon's latency log
//! (data/latency/latency-YYYY-MM-DD.jsonl, one JSON line per turn, written
//! when the daemon runs with LATENCY_LOG=1).
//!
//! The log's own `tts_s` is measured from the end of Claude's reply, so it
//! is empty whenever audio starts before the reply ends, which is most
//! turns. The widget uses the turn's `marks` instead and cuts the critical
//! path into segments that add up to `response_s` exactly:
//!
//! ```text
//! vad_stop ─ listen ─ transcript ─ handoff ─ llm_request ─ first word ─
//! llm_first_token ─ sentence ─ tts_first_audio ─ playback ─ bot_start
//! ```
//!
//! Pure functions, no capability calls: `cargo test` runs them natively.

use serde_json::Value;

/// One step of the critical path, in the order a turn goes through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The smart-turn wait plus parakeet transcribing the segment.
    Listen,
    /// The prompt going to Claude and its first word coming back.
    FirstWord,
    /// The rest of the first sentence plus pocket-tts voicing it.
    Sentence,
    /// Hand-off to Claude and the audio reaching the speakers.
    Glue,
}

pub const STAGES: [Stage; 4] = [
    Stage::Listen,
    Stage::FirstWord,
    Stage::Sentence,
    Stage::Glue,
];

impl Stage {
    /// The row label: what the stage is and what it covers.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Listen => "1. Listen: turn wait + transcription",
            Stage::FirstWord => "2. Claude: first word",
            Stage::Sentence => "3. First sentence written + voiced",
            Stage::Glue => "4. Glue: hand-off + playback",
        }
    }

    /// The short name the "Optimize first" row shows.
    pub fn short(self) -> &'static str {
        match self {
            Stage::Listen => "Listen",
            Stage::FirstWord => "Claude first word",
            Stage::Sentence => "First sentence",
            Stage::Glue => "Glue",
        }
    }

    /// Where to look when this stage is the biggest.
    pub fn hint(self) -> &'static str {
        match self {
            Stage::Listen => "smart-turn wait, parakeet",
            Stage::FirstWord => "prompt size, model, tools",
            Stage::Sentence => "sentence splitter, pocket-tts",
            Stage::Glue => "transport, audio buffers",
        }
    }
}

/// One user turn from the log, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// You stop talking -> the agent starts talking.
    pub response: f64,
    /// The critical path, in `STAGES` order, when every mark is there.
    pub stages: Option<[f64; 4]>,
    /// pocket-tts on its own, when the reply ended before the audio began.
    pub tts: Option<f64>,
}

fn mark(marks: &Value, key: &str) -> Option<f64> {
    marks.get(key)?.as_f64()
}

/// The critical path from a line's `marks`, or None when a mark is missing
/// or the marks run backwards.
fn stages(marks: &Value) -> Option<[f64; 4]> {
    let at = |k| mark(marks, k);
    let points = [
        at("vad_stop")?,
        at("transcript")?,
        at("llm_request")?,
        at("llm_first_token")?,
        at("tts_first_audio")?,
        at("bot_start")?,
    ];
    if points.windows(2).any(|w| w[1] < w[0]) {
        return None;
    }
    let gap = |i: usize| points[i + 1] - points[i];
    Some([gap(0), gap(2), gap(3), gap(1) + gap(4)])
}

/// The user turns in a latency log: lines with a `response_s`, not started
/// by the agent itself. Lines that are not JSON are skipped.
pub fn parse_turns(body: &[u8]) -> Vec<Turn> {
    String::from_utf8_lossy(body)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|doc| doc.get("autonomous").and_then(Value::as_bool) != Some(true))
        .filter_map(|doc| {
            let response = doc.get("response_s")?.as_f64()?;
            let marks = doc.get("marks").cloned().unwrap_or(Value::Null);
            Some(Turn {
                response,
                stages: stages(&marks),
                tts: doc.get("tts_s").and_then(Value::as_f64),
            })
        })
        .collect()
}

/// The newest "latency-YYYY-MM-DD.jsonl" in a directory listing.
pub fn latest_log(names: &[String]) -> Option<&str> {
    names
        .iter()
        .filter(|n| log_day(n).is_some())
        .max()
        .map(String::as_str)
}

/// "2026-11-19" from "latency-2026-11-19.jsonl".
pub fn log_day(name: &str) -> Option<&str> {
    let day = name.strip_prefix("latency-")?.strip_suffix(".jsonl")?;
    let b = day.as_bytes();
    let ok = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        });
    ok.then_some(day)
}

/// The value at quantile `q` (nearest rank), of a non-empty list.
pub fn quantile(values: &[f64], q: f64) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let rank = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len());
    v[rank - 1]
}

pub fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn secs(s: f64) -> String {
    format!("{s:.2} s")
}

/// The widget's rows for the last `window` turns of the log for `day`:
/// the end-to-end wait, each stage's median and share of it, pocket-tts on
/// its own, and the stage to work on first.
pub fn rows(turns: &[Turn], window: usize, day: &str) -> Vec<(String, String)> {
    let row = |l: &str, v: String| (l.to_string(), v);
    let recent = &turns[turns.len().saturating_sub(window)..];
    let Some(last) = recent.last() else {
        return vec![
            row("Turns", format!("none on {day}")),
            row("Measured", "after a call".into()),
        ];
    };
    let responses: Vec<f64> = recent.iter().map(|t| t.response).collect();
    let mut out = vec![
        row("End to end: you stop → it talks", secs(median(&responses))),
        row("End to end, slowest 10%", secs(quantile(&responses, 0.9))),
        row("Last turn", secs(last.response)),
    ];

    let full: Vec<[f64; 4]> = recent.iter().filter_map(|t| t.stages).collect();
    if !full.is_empty() {
        let medians: Vec<f64> = (0..STAGES.len())
            .map(|i| median(&full.iter().map(|s| s[i]).collect::<Vec<_>>()))
            .collect();
        let sum: f64 = medians.iter().sum();
        for (stage, m) in STAGES.iter().zip(&medians) {
            let share = if sum > 0.0 { m / sum * 100.0 } else { 0.0 };
            out.push(row(stage.label(), format!("{} · {share:.0}%", secs(*m))));
        }
        let tts: Vec<f64> = recent.iter().filter_map(|t| t.tts).collect();
        if !tts.is_empty() {
            out.push(row("pocket-tts alone (whole replies)", secs(median(&tts))));
        }
        let (worst, _) = STAGES
            .iter()
            .zip(&medians)
            .max_by(|a, b| a.1.total_cmp(b.1))
            .expect("four stages");
        out.push(row("Optimize first", worst.short().into()));
        out.push(row("Look at", worst.hint().into()));
    }
    out.push(row(
        "Turns",
        format!("{} of {} on {day}", recent.len(), turns.len()),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two real lines from data/latency: one whole reply, one where audio
    // starts before the reply ends (tts_s is null there).
    const WHOLE: &str = r#"{"turn":56,"autonomous":false,"listen_s":0.271,"llm_first_token_s":3.216,"llm_total_s":3.341,"tts_s":0.075,"response_s":3.689,"marks":{"speech_start":0.0,"vad_stop":2.319,"transcript":2.589,"llm_request":2.591,"llm_first_token":5.807,"llm_end":5.932,"tts_first_audio":6.008,"bot_start":6.008,"bot_stop":12.884}}"#;
    const STREAMED: &str = r#"{"turn":3,"autonomous":false,"tts_s":null,"response_s":1.737,"marks":{"speech_start":0.0,"vad_stop":4.38,"transcript":4.707,"llm_request":4.709,"llm_first_token":5.757,"llm_end":6.194,"tts_first_audio":6.117,"bot_start":6.117}}"#;
    const AUTONOMOUS: &str = r#"{"autonomous":true,"response_s":9.0,"marks":{}}"#;
    const NO_REPLY: &str = r#"{"autonomous":false,"response_s":null,"marks":{"speech_start":0.0}}"#;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn stages_add_up_to_the_response() {
        let turns = parse_turns(format!("{WHOLE}\n{STREAMED}\n").as_bytes());
        assert_eq!(turns.len(), 2);
        for t in &turns {
            let s = t.stages.expect("all marks");
            assert!(close(s.iter().sum::<f64>(), t.response), "{t:?}");
        }
        let s = turns[1].stages.unwrap();
        assert!(close(s[0], 4.707 - 4.38)); // listen
        assert!(close(s[1], 5.757 - 4.709)); // first word
        assert!(close(s[2], 6.117 - 5.757)); // sentence + voice
        assert_eq!(turns[0].tts, Some(0.075));
        assert_eq!(turns[1].tts, None);
    }

    #[test]
    fn skips_agent_turns_turns_without_a_reply_and_junk() {
        let body = format!("{AUTONOMOUS}\n{NO_REPLY}\nnot json\n\n{WHOLE}");
        assert_eq!(parse_turns(body.as_bytes()).len(), 1);
    }

    #[test]
    fn a_missing_or_backwards_mark_drops_only_the_breakdown() {
        let body = r#"{"response_s":2.0,"marks":{"vad_stop":1.0,"transcript":1.2}}
{"response_s":2.0,"marks":{"vad_stop":1.0,"transcript":0.5,"llm_request":1.3,"llm_first_token":2.0,"tts_first_audio":2.5,"bot_start":3.0}}"#;
        let turns = parse_turns(body.as_bytes());
        assert_eq!(turns.len(), 2);
        assert!(turns.iter().all(|t| t.stages.is_none()));
    }

    #[test]
    fn finds_the_newest_log() {
        let names = [
            "latency-2026-11-09.jsonl",
            "latency-2026-11-19.jsonl",
            "latency-2026-11-2.jsonl",
            "clean-2026-11-20.jsonl",
            "latency-2026-11-18.jsonl",
        ]
        .map(String::from);
        assert_eq!(latest_log(&names), Some("latency-2026-11-19.jsonl"));
        assert_eq!(latest_log(&[]), None);
        assert_eq!(log_day("latency-2026-11-19.jsonl"), Some("2026-11-19"));
        assert_eq!(log_day("latency-2026x11-19.jsonl"), None);
    }

    #[test]
    fn quantiles() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(median(&v), 3.0);
        assert_eq!(median(&[1.0, 2.0]), 1.5);
        assert_eq!(quantile(&v, 0.9), 5.0);
        assert_eq!(quantile(&v, 0.5), 3.0);
        assert_eq!(quantile(&[7.0], 0.9), 7.0);
    }

    #[test]
    fn rows_name_the_stage_to_optimize() {
        let turns = parse_turns(format!("{WHOLE}\n{STREAMED}").as_bytes());
        let rows = rows(&turns, 50, "2026-11-19");
        let get = |l: &str| {
            rows.iter()
                .find(|(label, _)| label == l)
                .map(|(_, v)| v.as_str())
                .unwrap_or_else(|| panic!("no row {l}: {rows:?}"))
        };
        assert_eq!(get("End to end: you stop → it talks"), "2.71 s");
        assert_eq!(get("End to end, slowest 10%"), "3.69 s");
        assert_eq!(get("Last turn"), "1.74 s");
        // First word: medians of 3.216 and 1.048.
        assert_eq!(get("2. Claude: first word"), "2.13 s · 79%");
        assert_eq!(get("pocket-tts alone (whole replies)"), "0.07 s");
        assert_eq!(get("Optimize first"), "Claude first word");
        assert_eq!(get("Turns"), "2 of 2 on 2026-11-19");
        assert!(rows.len() <= 16);
    }

    #[test]
    fn the_window_keeps_the_latest_turns() {
        let turns = parse_turns(format!("{WHOLE}\n{STREAMED}").as_bytes());
        let rows = rows(&turns, 1, "d");
        assert_eq!(rows[0].1, "1.74 s");
        assert_eq!(rows.last().unwrap().1, "1 of 2 on d");
    }

    #[test]
    fn says_when_there_is_nothing_yet() {
        let rows = rows(&[], 50, "2026-11-20");
        assert_eq!(rows[0], ("Turns".into(), "none on 2026-11-20".into()));
    }
}
