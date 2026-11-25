//! The Windows choices for `tts`: SAPI 5 speaks when `node.toml` names no
//! TTS server, and `ffplay` or PowerShell plays a server's WAV files.
//!
//! SAPI runs on one thread of its own, `bricolo-sapi`, which holds the COM
//! voice (`ISpVoice`). The queue in `tts.rs` hands it one chunk at a time;
//! the thread speaks it asynchronously and polls until it is done, or until
//! the queue dropped the chunk (`stop`), when it purges the speech at once.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use ::windows::Win32::Media::Speech::{
    IEnumSpObjectTokens, ISpObjectToken, ISpObjectTokenCategory, ISpVoice, SPCAT_VOICES, SPF_ASYNC,
    SPF_IS_NOT_XML, SPF_PURGEBEFORESPEAK, SPRS_DONE, SPVOICESTATUS, SpObjectTokenCategory, SpVoice,
};
use ::windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use ::windows::core::{HSTRING, PCWSTR, PWSTR, w};
use tokio::sync::oneshot;

use crate::tts::{Engine, match_voice, windows_player};

/// How often the SAPI thread checks for the end of a chunk and for `stop`.
const POLL_MS: u32 = 50;

/// The engine for a node without a TTS server.
pub fn engine() -> Option<Engine> {
    Some(Engine::Sapi)
}

/// The player for a TTS server's WAV files: `ffplay` on `PATH`, else
/// PowerShell's `SoundPlayer`.
pub fn player() -> Vec<String> {
    windows_player(which("ffplay.exe").as_deref())
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| Path::is_file(p))
}

struct Job {
    text: String,
    voice: Option<String>,
    cancel: Arc<AtomicBool>,
    reply: oneshot::Sender<Result<(), String>>,
}

/// Cancels the chunk when the queue drops its future, unless it finished.
struct CancelOnDrop(Arc<AtomicBool>, bool);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.1 {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

static JOBS: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);

/// Speaks `text` with SAPI and returns once it is spoken. Dropping the
/// future stops the speech.
pub async fn speak(text: &str, voice: Option<&str>) -> Result<(), String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut guard = CancelOnDrop(cancel.clone(), false);
    let (reply, answer) = oneshot::channel();
    let job = Job {
        text: text.to_string(),
        voice: voice.map(str::to_string),
        cancel,
        reply,
    };
    {
        let mut jobs = JOBS.lock().unwrap();
        let unsent = match jobs.as_ref() {
            Some(tx) => tx.send(job).err().map(|mpsc::SendError(job)| job),
            None => Some(job),
        };
        if let Some(job) = unsent {
            // First call, or the thread died: start it.
            let tx = start()?;
            tx.send(job)
                .map_err(|_| "the SAPI thread stopped".to_string())?;
            *jobs = Some(tx);
        }
    }
    let result = answer
        .await
        .map_err(|_| "the SAPI thread stopped".to_string())?;
    guard.1 = true;
    result
}

fn start() -> Result<mpsc::Sender<Job>, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("bricolo-sapi".into())
        .spawn(move || serve(rx))
        .map_err(|e| format!("cannot start the SAPI thread: {e}"))?;
    Ok(tx)
}

fn serve(jobs: mpsc::Receiver<Job>) {
    // SAFETY: COM for this thread, which owns the voice.
    let voice: Result<ISpVoice, String> = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&SpVoice, None, CLSCTX_ALL).map_err(|e| format!("SAPI: {e}"))
    };
    for job in jobs {
        let result = match &voice {
            Ok(voice) => speak_job(voice, &job),
            Err(e) => Err(e.clone()),
        };
        let _ = job.reply.send(result);
    }
}

fn speak_job(voice: &ISpVoice, job: &Job) -> Result<(), String> {
    // SAFETY: COM calls on this thread's voice; every string outlives its
    // call.
    unsafe {
        if job.cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let token = match &job.voice {
            Some(name) => Some(find_voice(name)?),
            None => None,
        };
        // None goes back to the user's default voice.
        voice
            .SetVoice(token.as_ref())
            .map_err(|e| format!("SAPI voice: {e}"))?;
        let text = HSTRING::from(job.text.as_str());
        let flags = (SPF_ASYNC.0 | SPF_IS_NOT_XML.0 | SPF_PURGEBEFORESPEAK.0) as u32;
        voice
            .Speak(&text, flags, None)
            .map_err(|e| format!("SAPI: {e}"))?;
        loop {
            let _ = voice.WaitUntilDone(POLL_MS);
            if job.cancel.load(Ordering::Relaxed) {
                let _ = voice.Speak(PCWSTR::null(), SPF_PURGEBEFORESPEAK.0 as u32, None);
                return Err("stopped".into());
            }
            let mut status = SPVOICESTATUS::default();
            voice
                .GetStatus(&mut status, std::ptr::null_mut())
                .map_err(|e| format!("SAPI: {e}"))?;
            if status.dwRunningState == SPRS_DONE.0 as u32 {
                return status.hrLastResult.ok().map_err(|e| format!("SAPI: {e}"));
            }
        }
    }
}

/// The installed voice that `name` names, by [`match_voice`] over each
/// voice's `Name` attribute, else its description.
unsafe fn find_voice(name: &str) -> Result<ISpObjectToken, String> {
    // SAFETY: COM calls on this thread; strings SAPI returns are freed.
    unsafe {
        let category: ISpObjectTokenCategory =
            CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL)
                .map_err(|e| format!("SAPI voices: {e}"))?;
        category
            .SetId(SPCAT_VOICES, false)
            .map_err(|e| format!("SAPI voices: {e}"))?;
        let tokens: IEnumSpObjectTokens = category
            .EnumTokens(PCWSTR::null(), PCWSTR::null())
            .map_err(|e| format!("SAPI voices: {e}"))?;
        let mut found: Vec<(String, ISpObjectToken)> = Vec::new();
        loop {
            let mut token: [Option<ISpObjectToken>; 1] = [None];
            let mut fetched = 0u32;
            if tokens
                .Next(1, token.as_mut_ptr(), Some(&mut fetched))
                .is_err()
                || fetched == 0
            {
                break;
            }
            let Some(token) = token[0].take() else {
                break;
            };
            let label = token
                .OpenKey(w!("Attributes"))
                .and_then(|attrs| attrs.GetStringValue(w!("Name")))
                .or_else(|_| token.GetStringValue(PCWSTR::null()))
                .map(|s| take_string(s))
                .unwrap_or_default();
            found.push((label, token));
        }
        let labels: Vec<String> = found.iter().map(|(l, _)| l.clone()).collect();
        match match_voice(&labels, name) {
            Some(i) => Ok(found.swap_remove(i).1),
            None => Err(format!(
                "no SAPI voice named {name:?}; installed: {}",
                labels.join(", ")
            )),
        }
    }
}

/// Copies a string SAPI allocated and frees it.
unsafe fn take_string(s: PWSTR) -> String {
    // SAFETY: `s` is a NUL-terminated string from CoTaskMemAlloc.
    unsafe {
        let text = s.to_string().unwrap_or_default();
        CoTaskMemFree(Some(s.0 as *const _));
        text
    }
}
