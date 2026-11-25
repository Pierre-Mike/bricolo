//! `audio` through AVAudioPlayer: no ffmpeg, loops without a gap, and the
//! volume changes while the sound plays. The players live on one thread,
//! "bricolo-audio", because AppKit objects are not `Send`; a [`MacVoice`] is a
//! handle that sends that thread commands. AVAudioPlayer plays on its own
//! audio threads, so this one needs no run loop.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};

use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_avf_audio::AVAudioPlayer;
use objc2_foundation::{NSString, NSURL};

use crate::audio::{Backend, PlayOptions, Voice};
use crate::error::CallError;

type Reply<T> = Sender<Result<T, CallError>>;

enum Command {
    Start {
        key: u64,
        path: String,
        options: PlayOptions,
        reply: Reply<()>,
    },
    Volume {
        key: u64,
        volume: u8,
        reply: Reply<()>,
    },
    Playing {
        key: u64,
        reply: Sender<bool>,
    },
    Stop {
        key: u64,
    },
}

/// The handle on the "bricolo-audio" thread.
pub struct AvPlayer {
    commands: Sender<Command>,
    next: AtomicU64,
}

impl AvPlayer {
    /// Starts the "bricolo-audio" thread.
    pub fn new() -> Self {
        let (commands, inbox) = channel();
        std::thread::Builder::new()
            .name("bricolo-audio".into())
            .spawn(move || {
                let mut players: HashMap<u64, Retained<AVAudioPlayer>> = HashMap::new();
                while let Ok(command) = inbox.recv() {
                    autoreleasepool(|_| run(&mut players, command));
                }
            })
            .expect("cannot start the bricolo-audio thread");
        Self {
            commands,
            next: AtomicU64::new(1),
        }
    }
}

impl Default for AvPlayer {
    fn default() -> Self {
        Self::new()
    }
}

/// AVAudioPlayer's volume: 0.0 to 1.0.
fn gain(volume: u8) -> f32 {
    f32::from(volume.min(100)) / 100.0
}

fn open(path: &str, options: PlayOptions) -> Result<Retained<AVAudioPlayer>, CallError> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    // SAFETY: plain AVAudioPlayer calls on the thread that owns it.
    unsafe {
        let player = AVAudioPlayer::initWithContentsOfURL_error(AVAudioPlayer::alloc(), &url)
            .map_err(|e| CallError::Failed(e.localizedDescription().to_string()))?;
        player.setNumberOfLoops(if options.looping { -1 } else { 0 });
        player.setVolume(gain(options.volume));
        player.prepareToPlay();
        if !player.play() {
            return Err(CallError::Failed(format!(
                "AVAudioPlayer cannot play {path}"
            )));
        }
        Ok(player)
    }
}

fn run(players: &mut HashMap<u64, Retained<AVAudioPlayer>>, command: Command) {
    match command {
        Command::Start {
            key,
            path,
            options,
            reply,
        } => {
            let result = open(&path, options).map(|player| {
                players.insert(key, player);
            });
            let _ = reply.send(result);
        }
        Command::Volume { key, volume, reply } => {
            let result = match players.get(&key) {
                // SAFETY: as in `open`.
                Some(player) => {
                    unsafe { player.setVolume(gain(volume)) };
                    Ok(())
                }
                None => Err(CallError::Failed("the sound has ended".into())),
            };
            let _ = reply.send(result);
        }
        Command::Playing { key, reply } => {
            // SAFETY: as in `open`.
            let playing = players.get(&key).is_some_and(|p| unsafe { p.isPlaying() });
            if !playing {
                players.remove(&key);
            }
            let _ = reply.send(playing);
        }
        Command::Stop { key } => {
            if let Some(player) = players.remove(&key) {
                // SAFETY: as in `open`.
                unsafe { player.stop() };
            }
        }
    }
}

fn gone(_: impl std::fmt::Debug) -> CallError {
    CallError::Failed("the bricolo-audio thread has stopped".into())
}

impl Backend for AvPlayer {
    fn start(&self, path: &Path, options: PlayOptions) -> Result<Box<dyn Voice>, CallError> {
        let key = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = channel();
        self.commands
            .send(Command::Start {
                key,
                path: path.to_string_lossy().into_owned(),
                options,
                reply,
            })
            .map_err(gone)?;
        answer.recv().map_err(gone)??;
        Ok(Box::new(MacVoice {
            key,
            commands: self.commands.clone(),
        }))
    }
}

/// One player on the "bricolo-audio" thread. Dropping it stops the player.
pub struct MacVoice {
    key: u64,
    commands: Sender<Command>,
}

impl Voice for MacVoice {
    fn set_volume(&mut self, volume: u8) -> Result<(), CallError> {
        let (reply, answer) = channel();
        self.commands
            .send(Command::Volume {
                key: self.key,
                volume,
                reply,
            })
            .map_err(gone)?;
        answer.recv().map_err(gone)?
    }

    fn playing(&mut self) -> bool {
        let (reply, answer) = channel();
        let sent = self.commands.send(Command::Playing {
            key: self.key,
            reply,
        });
        sent.is_ok() && answer.recv().unwrap_or(false)
    }
}

impl Drop for MacVoice {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop { key: self.key });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_audio_fails_to_open() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.wav");
        std::fs::write(&bad, "not audio").unwrap();
        let player = AvPlayer::new();
        let r = player.start(&bad, PlayOptions::default());
        assert!(matches!(r, Err(CallError::Failed(_))));
    }

    /// Plays a quarter second of silence through the real output, and
    /// checks that it plays, then ends by itself. Needs an audio output
    /// device, which CI machines may lack.
    #[ignore = "needs an audio output device"]
    #[test]
    fn real_output_plays_and_ends() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("silence.wav");
        std::fs::write(&wav, silent_wav(16_000, 4_000)).unwrap();
        let player = AvPlayer::new();
        let mut voice = player
            .start(
                &wav,
                PlayOptions {
                    volume: 0,
                    looping: false,
                },
            )
            .unwrap();
        assert!(voice.playing());
        voice.set_volume(10).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(!voice.playing(), "a quarter second of sound still plays");

        let mut looped = player
            .start(
                &wav,
                PlayOptions {
                    volume: 0,
                    looping: true,
                },
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        assert!(looped.playing(), "a looping sound ended");
    }

    /// A mono 16-bit WAV of `samples` zeros at `rate`.
    fn silent_wav(rate: u32, samples: u32) -> Vec<u8> {
        let data = samples * 2;
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + data).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(1u16.to_le_bytes()); // mono
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 2).to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data.to_le_bytes());
        wav.resize(wav.len() + data as usize, 0);
        wav
    }
}
