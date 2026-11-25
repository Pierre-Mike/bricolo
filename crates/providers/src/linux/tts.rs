//! The Linux choices for `tts`: which speech engine speaks when `node.toml`
//! names no TTS server, and which player plays the server's WAV files.

use std::path::PathBuf;

use super::{first_found, which};
use crate::tts::Engine;

/// Engines in order of preference. `espeak-ng` and `espeak` take the text
/// on stdin and `-v voice`, like macOS `say`; `spd-say` goes through Speech
/// Dispatcher.
const ENGINES: [&str; 3] = ["espeak-ng", "espeak", "spd-say"];

/// Players in order of preference: PipeWire, then PulseAudio (which
/// PipeWire also serves), then plain ALSA.
const PLAYERS: [&str; 3] = ["pw-play", "paplay", "aplay"];

/// The engine for a node without a TTS server, found with `find`.
pub fn engine_with(find: impl Fn(&str) -> Option<PathBuf>) -> Option<Engine> {
    let path = first_found(&ENGINES, find)?;
    let program = path.to_string_lossy().into_owned();
    Some(if path.file_name().is_some_and(|n| n == "spd-say") {
        Engine::SpdSay { program }
    } else {
        Engine::Say { program }
    })
}

/// The player for a TTS server's WAV files, found with `find`; `aplay` by
/// name when none is installed, so the error names a program.
pub fn player_with(find: impl Fn(&str) -> Option<PathBuf>) -> Vec<String> {
    let player = first_found(&PLAYERS, find)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "aplay".into());
    vec![player]
}

/// [`engine_with`] on `PATH`.
pub fn engine() -> Option<Engine> {
    engine_with(which)
}

/// [`player_with`] on `PATH`.
pub fn player() -> Vec<String> {
    player_with(which)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn installed(names: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| {
            names
                .contains(&name)
                .then(|| Path::new("/usr/bin").join(name))
        }
    }

    #[test]
    fn players_prefer_pipewire_then_pulse_then_alsa() {
        assert_eq!(
            player_with(installed(&["aplay", "paplay", "pw-play"])),
            ["/usr/bin/pw-play"]
        );
        assert_eq!(
            player_with(installed(&["aplay", "paplay"])),
            ["/usr/bin/paplay"]
        );
        assert_eq!(player_with(installed(&["aplay"])), ["/usr/bin/aplay"]);
        assert_eq!(player_with(installed(&[])), ["aplay"]);
    }

    #[test]
    fn engines_prefer_espeak_ng() {
        assert_eq!(
            engine_with(installed(&["spd-say", "espeak-ng"])),
            Some(Engine::Say {
                program: "/usr/bin/espeak-ng".into()
            })
        );
        assert_eq!(
            engine_with(installed(&["spd-say"])),
            Some(Engine::SpdSay {
                program: "/usr/bin/spd-say".into()
            })
        );
        assert_eq!(engine_with(installed(&[])), None);
    }
}
