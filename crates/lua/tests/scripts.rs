//! Runs real Lua scripts through the script tier, behind the real router.
//! The desktop capabilities, `tts` and `speech` are fakes, so the tests need
//! no GUI session, permission or server. `shell` and `http` are the real
//! portable providers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bricolo_lua::{Limits, Mailbox, Options, ScriptInfo, Scripts, State};
use bricolo_providers::app::{AppInfo, AppProvider};
use bricolo_providers::audio::{Backend, PlayOptions, Player, Voice};
use bricolo_providers::camera::CameraProvider;
use bricolo_providers::claude_code::{ClaudeCodeProvider, Session, SessionState};
use bricolo_providers::clipboard::ClipboardProvider;
use bricolo_providers::disk::LocalDisk;
use bricolo_providers::files::NotifyFiles;
use bricolo_providers::hotkey::HotkeyProvider;
use bricolo_providers::http::LocalHttp;
use bricolo_providers::keyboard::KeyboardProvider;
use bricolo_providers::kv::FileKv;
use bricolo_providers::log::TracingLog;
use bricolo_providers::mic::{MicProvider, RecordOptions};
use bricolo_providers::mouse::{MouseButton, MouseProvider, Point};
use bricolo_providers::notify::NotifyProvider;
use bricolo_providers::screen::ScreenProvider;
use bricolo_providers::secrets::{MemoryStore, Secrets};
use bricolo_providers::selection::SelectionProvider;
use bricolo_providers::shell::LocalShell;
use bricolo_providers::sidecar::Sidecars;
use bricolo_providers::speech::SpeechProvider;
use bricolo_providers::system::{Power, SystemProvider};
use bricolo_providers::timer::LocalTimers;
use bricolo_providers::tts::{TtsProvider, TtsStatus};
use bricolo_providers::ui::{LocalUi, PanelKind, UiAction};
use bricolo_providers::window::{Rect, Screen, WindowInfo, WindowProvider};
use bricolo_providers::{
    AppChange, AppEvent, CallError, Caller, ProviderEvent, Providers, SystemEvent,
};
use bricolo_router::{NoPeers, Rules};
use tempfile::TempDir;

/// Records what it was asked to say.
#[derive(Default)]
struct FakeTts(Mutex<Vec<String>>);

#[async_trait]
impl TtsProvider for FakeTts {
    async fn speak(&self, _: &Caller, text: String, _: Option<String>) -> Result<(), CallError> {
        self.0.lock().unwrap().push(text);
        Ok(())
    }
    async fn stop(&self, _: &Caller) -> Result<(), CallError> {
        Ok(())
    }
    async fn status(&self, _: &Caller) -> Result<TtsStatus, CallError> {
        Ok(TtsStatus {
            speaking: false,
            queued: self.0.lock().unwrap().len() as u32,
            voice: Some("alba".into()),
        })
    }
}

/// Transcribes the audio `ok` after a short wait; refuses anything else.
struct FakeSpeech;

#[async_trait]
impl SpeechProvider for FakeSpeech {
    async fn transcribe(
        &self,
        _: &Caller,
        audio: Vec<u8>,
        format: String,
    ) -> Result<String, CallError> {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if audio == b"ok" {
            Ok(format!("hello world ({format})"))
        } else {
            Err(CallError::Failed("bad audio".into()))
        }
    }

    async fn transcribe_file(&self, _: &Caller, path: String) -> Result<String, CallError> {
        Ok(format!(
            " heard {} ",
            path.rsplit('/').next().unwrap_or_default()
        ))
    }
}

/// Records calls; the pointer starts at (5, 6).
struct FakeMouse(Mutex<Vec<String>>, Mutex<Point>);

impl Default for FakeMouse {
    fn default() -> Self {
        Self(Mutex::default(), Mutex::new(Point { x: 5, y: 6 }))
    }
}

#[async_trait]
impl MouseProvider for FakeMouse {
    async fn position(&self, _: &Caller) -> Result<Point, CallError> {
        Ok(*self.1.lock().unwrap())
    }
    async fn move_to(&self, _: &Caller, to: Point) -> Result<(), CallError> {
        *self.1.lock().unwrap() = to;
        self.0
            .lock()
            .unwrap()
            .push(format!("move {} {}", to.x, to.y));
        Ok(())
    }
    async fn click(&self, _: &Caller, button: MouseButton, count: u8) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("click {button:?} {count}"));
        Ok(())
    }
    async fn scroll(&self, _: &Caller, dx: i32, dy: i32) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("scroll {dx} {dy}"));
        Ok(())
    }
}

/// Records each area; always "writes" `shot_path()`.
#[derive(Default)]
struct FakeScreen(Mutex<Vec<Option<Rect>>>);

#[async_trait]
impl ScreenProvider for FakeScreen {
    async fn capture(&self, _: &Caller, area: Option<Rect>) -> Result<String, CallError> {
        self.0.lock().unwrap().push(area);
        Ok(shot_path())
    }
}

/// "hello" is selected.
struct FakeSelection;

#[async_trait]
impl SelectionProvider for FakeSelection {
    async fn get_text(&self, _: &Caller) -> Result<Option<String>, CallError> {
        Ok(Some("hello".into()))
    }
}

/// Records keystrokes as "mods+key" and typed text as "type:<text>".
#[derive(Default)]
struct FakeKeyboard(Mutex<Vec<String>>);

#[async_trait]
impl KeyboardProvider for FakeKeyboard {
    async fn keystroke(&self, _: &Caller, mods: Vec<String>, key: String) -> Result<(), CallError> {
        let mut chord = mods;
        chord.push(key);
        self.0.lock().unwrap().push(chord.join("+"));
        Ok(())
    }

    async fn type_text(&self, _: &Caller, text: String) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("type:{text}"));
        Ok(())
    }
}

/// One session, "s1", whatever is asked.
struct FakeClaudeCode;

fn fake_session(dir: String, state: SessionState) -> Session {
    Session {
        id: "s1".into(),
        dir,
        state,
        last_result: None,
        title: "fix it".into(),
        started: 10,
        updated: 11,
    }
}

#[async_trait]
impl ClaudeCodeProvider for FakeClaudeCode {
    async fn start(&self, _: &Caller, dir: String, _: String) -> Result<Session, CallError> {
        Ok(fake_session(dir, SessionState::Working))
    }
    async fn send(&self, _: &Caller, id: String, _: String) -> Result<Session, CallError> {
        Err(CallError::Failed(format!("{id} is working")))
    }
    async fn list(&self, _: &Caller) -> Result<Vec<Session>, CallError> {
        Ok(vec![fake_session("/w".into(), SessionState::Finished)])
    }
    async fn status(&self, _: &Caller, _: String) -> Result<Session, CallError> {
        let mut s = fake_session("/w".into(), SessionState::Waiting);
        s.last_result = Some("need Bash".into());
        Ok(s)
    }
    async fn stop(&self, _: &Caller, _: String) -> Result<(), CallError> {
        Ok(())
    }
}

struct FakeCamera;

#[async_trait]
impl CameraProvider for FakeCamera {
    async fn snapshot(&self, _: &Caller) -> Result<String, CallError> {
        Ok("/tmp/cam.jpg".into())
    }
}

/// Recordings by id: (caller, max_secs, still running).
#[derive(Default)]
struct FakeMic(Mutex<Vec<(String, u32, bool)>>);

#[async_trait]
impl MicProvider for FakeMic {
    async fn record_start(&self, c: &Caller, o: RecordOptions) -> Result<u64, CallError> {
        let mut recs = self.0.lock().unwrap();
        recs.push((c.plugin.clone(), o.max_secs, true));
        Ok(recs.len() as u64)
    }

    async fn record_stop(&self, c: &Caller, id: u64) -> Result<String, CallError> {
        let mut recs = self.0.lock().unwrap();
        match recs.get_mut(id as usize - 1) {
            Some(r) if r.0 == c.plugin && r.2 => {
                r.2 = false;
                Ok(format!("/data/{}/recordings/rec-{id}.wav", c.plugin))
            }
            _ => Err(CallError::Failed(format!("no recording {id}"))),
        }
    }

    async fn record_cancel(&self, c: &Caller, id: u64) -> Result<(), CallError> {
        self.record_stop(c, id).await.map(|_| ())
    }
}

impl FakeMic {
    /// The callers still recording.
    fn active(&self) -> Vec<String> {
        let recs = self.0.lock().unwrap();
        recs.iter().filter(|r| r.2).map(|r| r.0.clone()).collect()
    }
}

/// The real `audio` player over voices that play until stopped. `live`
/// holds "<file> <volume>" for each sound still playing.
#[derive(Default)]
struct FakeSpeakers {
    live: Arc<Mutex<Vec<String>>>,
}

struct FakeVoice {
    file: String,
    volume: u8,
    live: Arc<Mutex<Vec<String>>>,
}

impl FakeVoice {
    fn line(&self) -> String {
        format!("{} {}", self.file, self.volume)
    }
}

impl Backend for FakeSpeakers {
    fn start(&self, path: &std::path::Path, o: PlayOptions) -> Result<Box<dyn Voice>, CallError> {
        let voice = FakeVoice {
            file: path.file_name().unwrap().to_string_lossy().into_owned(),
            volume: o.volume,
            live: self.live.clone(),
        };
        self.live.lock().unwrap().push(voice.line());
        Ok(Box::new(voice))
    }
}

impl Voice for FakeVoice {
    fn set_volume(&mut self, volume: u8) -> Result<(), CallError> {
        let mut live = self.live.lock().unwrap();
        let old = self.line();
        self.volume = volume;
        live.retain(|l| l != &old);
        live.push(self.line());
        Ok(())
    }

    fn playing(&mut self) -> bool {
        true
    }
}

impl Drop for FakeVoice {
    fn drop(&mut self) {
        let line = self.line();
        self.live.lock().unwrap().retain(|l| l != &line);
    }
}

/// Records bindings as (caller, id, chord).
#[derive(Default)]
struct FakeHotkeys(Mutex<Vec<(String, String, String)>>);

#[async_trait]
impl HotkeyProvider for FakeHotkeys {
    async fn bind(&self, caller: &Caller, id: String, chord: String) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .push((caller.plugin.clone(), id, chord));
        Ok(())
    }
    async fn unbind(&self, caller: &Caller, id: String) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .retain(|(c, i, _)| !(c == &caller.plugin && i == &id));
        Ok(())
    }
    fn release(&self, plugin: &str) {
        self.0.lock().unwrap().retain(|(c, _, _)| c != plugin);
    }
}

#[derive(Default)]
struct FakeClipboard(Mutex<Option<String>>);

#[async_trait]
impl ClipboardProvider for FakeClipboard {
    async fn get(&self, _: &Caller) -> Result<Option<String>, CallError> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn set(&self, _: &Caller, text: String) -> Result<(), CallError> {
        *self.0.lock().unwrap() = Some(text);
        Ok(())
    }
}

/// One window on one screen; `set_frame` moves it.
struct FakeWindows(Mutex<WindowInfo>, Mutex<Vec<String>>);

impl Default for FakeWindows {
    fn default() -> Self {
        Self(
            Mutex::new(WindowInfo {
                id: 7,
                app: "Notes".into(),
                title: "todo".into(),
                frame: Rect {
                    x: 10,
                    y: 20,
                    width: 300,
                    height: 200,
                },
            }),
            Mutex::default(),
        )
    }
}

#[async_trait]
impl WindowProvider for FakeWindows {
    async fn list(&self, _: &Caller) -> Result<Vec<WindowInfo>, CallError> {
        Ok(vec![self.0.lock().unwrap().clone()])
    }
    async fn focused(&self, _: &Caller) -> Result<Option<WindowInfo>, CallError> {
        Ok(Some(self.0.lock().unwrap().clone()))
    }
    async fn focus(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        match id {
            7 => Ok(()),
            _ => Err(CallError::Failed("no such window".into())),
        }
    }
    async fn set_frame(&self, _: &Caller, id: u64, frame: Rect) -> Result<(), CallError> {
        self.focus(&Caller::default(), id).await?;
        self.0.lock().unwrap().frame = frame;
        Ok(())
    }
    async fn close(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        self.focus(&Caller::default(), id).await?;
        self.1.lock().unwrap().push(format!("close {id}"));
        Ok(())
    }
    async fn set_minimized(&self, _: &Caller, id: u64, minimized: bool) -> Result<(), CallError> {
        self.focus(&Caller::default(), id).await?;
        self.1
            .lock()
            .unwrap()
            .push(format!("minimized {id} {minimized}"));
        Ok(())
    }
    async fn screens(&self, _: &Caller) -> Result<Vec<Screen>, CallError> {
        let frame = Rect {
            x: 0,
            y: 0,
            width: 1440,
            height: 900,
        };
        Ok(vec![Screen {
            id: 1,
            frame,
            visible_frame: Rect {
                y: 25,
                height: 875,
                ..frame
            },
            main: true,
        }])
    }
}

/// Records what it launched and opened; Finder is in front.
#[derive(Default)]
struct FakeApps(Mutex<Vec<String>>);

#[async_trait]
impl AppProvider for FakeApps {
    async fn launch(&self, _: &Caller, app: String) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("launch {app}"));
        Ok(())
    }
    async fn open_url(&self, _: &Caller, url: String) -> Result<(), CallError> {
        bricolo_providers::app::check_url(&url)?;
        self.0.lock().unwrap().push(format!("open {url}"));
        Ok(())
    }
    async fn running(&self, c: &Caller) -> Result<Vec<AppInfo>, CallError> {
        Ok(self.frontmost(c).await?.into_iter().collect())
    }
    async fn frontmost(&self, _: &Caller) -> Result<Option<AppInfo>, CallError> {
        Ok(Some(AppInfo {
            name: "Finder".into(),
            bundle_id: Some("com.apple.finder".into()),
            pid: 42,
        }))
    }
    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("watch {} {on}", caller.plugin));
        Ok(())
    }
    async fn quit(&self, _: &Caller, app: String) -> Result<(), CallError> {
        self.0.lock().unwrap().push(format!("quit {app}"));
        Ok(())
    }
    async fn set_hidden(&self, _: &Caller, app: String, hidden: bool) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("hidden {app} {hidden}"));
        Ok(())
    }
}

/// Idle 42 s, 87% on mains; records keep-awake, watch and release.
#[derive(Default)]
struct FakeSystem(Mutex<Vec<String>>);

#[async_trait]
impl SystemProvider for FakeSystem {
    async fn idle_seconds(&self, _: &Caller) -> Result<u64, CallError> {
        Ok(42)
    }
    async fn power(&self, _: &Caller) -> Result<Power, CallError> {
        Ok(Power {
            battery: Some(87),
            plugged_in: true,
        })
    }
    async fn keep_awake(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("awake {} {on}", caller.plugin));
        Ok(())
    }
    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        self.0
            .lock()
            .unwrap()
            .push(format!("watch {} {on}", caller.plugin));
        Ok(())
    }
    fn release(&self, plugin: &str) {
        self.0.lock().unwrap().push(format!("release {plugin}"));
    }
}

/// Records (title, body).
#[derive(Default)]
struct FakeNotify(Mutex<Vec<(String, String)>>);

#[async_trait]
impl NotifyProvider for FakeNotify {
    async fn show(&self, _: &Caller, title: String, body: String) -> Result<(), CallError> {
        self.0.lock().unwrap().push((title, body));
        Ok(())
    }
}

struct Node {
    _tmp: TempDir,
    root: PathBuf,
    dir: PathBuf,
    mailbox: Mailbox,
    providers: Providers,
    tts: Arc<FakeTts>,
    hotkeys: Arc<FakeHotkeys>,
    windows: Arc<FakeWindows>,
    apps: Arc<FakeApps>,
    system: Arc<FakeSystem>,
    notes: Arc<FakeNotify>,
    keys: Arc<FakeKeyboard>,
    mouse: Arc<FakeMouse>,
    screen: Arc<FakeScreen>,
    mic: Arc<FakeMic>,
    speakers: Arc<Mutex<Vec<String>>>,
    clipboard: Arc<FakeClipboard>,
    scripts: Option<Arc<Scripts>>,
    disabled: std::collections::BTreeSet<String>,
}

/// Drops the `\\?\` prefix `canonicalize` adds on Windows: a verbatim path
/// does not accept the `/` that scripts join onto it.
fn plain_path(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

/// Where the fake screen "writes" a capture: a folder that exists.
fn shot_path() -> String {
    if cfg!(windows) {
        std::env::temp_dir()
            .join("shot.png")
            .to_string_lossy()
            .into_owned()
    } else {
        "/tmp/shot.png".into()
    }
}

impl Node {
    fn new() -> Self {
        Self::with_rules("")
    }

    /// `rules` is a `rules.toml`; every peer is offline.
    fn with_rules(rules: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = plain_path(tmp.path().canonicalize().unwrap());
        let dir = root.join("scripts");
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        let mailbox = Mailbox::default();
        let tts = Arc::new(FakeTts::default());
        let hotkeys = Arc::new(FakeHotkeys::default());
        let windows = Arc::new(FakeWindows::default());
        let apps = Arc::new(FakeApps::default());
        let system = Arc::new(FakeSystem::default());
        let notes = Arc::new(FakeNotify::default());
        let keys = Arc::new(FakeKeyboard::default());
        let mouse = Arc::new(FakeMouse::default());
        let screen = Arc::new(FakeScreen::default());
        let mic = Arc::new(FakeMic::default());
        let clipboard = Arc::new(FakeClipboard::default());
        let speakers = FakeSpeakers::default();
        let live = speakers.live.clone();
        let local = Providers {
            log: Arc::new(TracingLog::default()),
            timer: Arc::new(LocalTimers::new(mailbox.sink())),
            kv: Arc::new(FileKv::new(root.join("data"))),
            disk: Arc::new(LocalDisk),
            tts: tts.clone(),
            speech: Arc::new(FakeSpeech),
            hotkey: hotkeys.clone(),
            clipboard: clipboard.clone(),
            window: windows.clone(),
            app: apps.clone(),
            notify: notes.clone(),
            shell: Arc::new(LocalShell::default()),
            http: Arc::new(LocalHttp),
            ui: Arc::new(LocalUi::new(mailbox.sink())),
            keyboard: keys.clone(),
            mouse: mouse.clone(),
            mic: mic.clone(),
            audio: Arc::new(Player::new(Arc::new(speakers))),
            system: system.clone(),
            files: Arc::new(NotifyFiles::new(mailbox.sink())),
            screen: screen.clone(),
            selection: Arc::new(FakeSelection),
            camera: Arc::new(FakeCamera),
            secrets: Arc::new(Secrets::new(MemoryStore::default())),
            claude_code: Arc::new(FakeClaudeCode),
            accessibility: Arc::new(fake_accessibility()),
            process: bricolo_providers::process::local(),
            device: Arc::new(bricolo_providers::device::Device::new(
                bricolo_providers::device::Os::Linux,
                fake_runner(),
                Box::new(bricolo_providers::device::NoNative),
                root.clone(),
            )),
            scripting: Arc::new(bricolo_providers::scripting::Scripting::new(
                bricolo_providers::device::Os::MacOs,
                fake_runner(),
                root.join("scripting"),
            )),
            virtual_screen: Arc::new(bricolo_providers::virtual_screen::NoVirtualScreen),
            sidecars: Arc::new(Sidecars::none()),
            sensors: Arc::default(),
        };
        let rules = Rules::parse(rules).unwrap();
        let providers = bricolo_router::route(&rules, local, Arc::new(NoPeers));
        Self {
            _tmp: tmp,
            root,
            dir,
            mailbox,
            providers,
            tts,
            hotkeys,
            windows,
            apps,
            system,
            notes,
            keys,
            mouse,
            screen,
            mic,
            speakers: live,
            clipboard,
            scripts: None,
            disabled: Default::default(),
        }
    }

    fn write(&self, rel: &str, source: &str) {
        let path = self.dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    async fn start(&mut self, watch: bool) -> Arc<Scripts> {
        self.start_with(watch, Limits::default()).await
    }

    async fn start_with(&mut self, watch: bool, limits: Limits) -> Arc<Scripts> {
        let scripts = Scripts::start(
            self.providers.clone(),
            self.mailbox.clone(),
            Options {
                dir: self.dir.clone(),
                watch,
                limits,
                disabled: self.disabled.clone(),
            },
        )
        .await
        .unwrap();
        self.scripts = Some(scripts.clone());
        scripts
    }

    fn lines(&self, script: &str) -> Vec<String> {
        self.providers
            .log
            .recent(Some(&format!("{script}.lua")), 1000)
            .into_iter()
            .map(|l| l.message)
            .collect()
    }

    /// Waits up to `limit` for `script` to log a line equal to `line`.
    async fn wait_line(&self, script: &str, line: &str, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if self.lines(script).iter().any(|l| l == line) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    async fn expect_line(&self, script: &str, line: &str) {
        assert!(
            self.wait_line(script, line, Duration::from_secs(5)).await,
            "{script} never logged {line:?}; it logged {:?}; status {:?}",
            self.lines(script),
            self.info(script).await,
        );
    }

    async fn info(&self, script: &str) -> Option<ScriptInfo> {
        self.scripts.as_ref()?.script_status(script).await
    }

    async fn wait_state(&self, script: &str, state: State) -> ScriptInfo {
        let start = Instant::now();
        loop {
            let info = self.info(script).await;
            if info.as_ref().is_some_and(|i| i.state == state) {
                return info.unwrap();
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{script} never reached {state:?}: {info:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_removes_system_access() {
    let mut node = Node::new();
    std::fs::write(node.root.join("outside.lua"), "return 1").unwrap();
    node.write(
        "sandbox.lua",
        r#"
local checks = {
  io = io == nil,
  debug = debug == nil,
  execute = os.execute == nil,
  exit = os.exit == nil,
  remove = os.remove == nil,
  rename = os.rename == nil,
  tmpname = os.tmpname == nil,
  loadlib = package.loadlib == nil,
  searchers = package.searchers == nil,
  time = type(os.time()) == "number",
  outside = not pcall(require, "..outside"),
  missing = not pcall(require, "socket"),
  dofile = not pcall(dofile, "../outside.lua"),
  loadfile = loadfile("../outside.lua") == nil,
  bytecode = load(string.dump(function() return 1 end)) == nil,
  source = load("return 41 + 1")() == 42,
  globals = load("return bricolo ~= nil")() == true,
  stdlib = require("string") == string,
}
local failed = {}
for name, ok in pairs(checks) do
  if not ok then failed[#failed + 1] = name end
end
table.sort(failed)
bricolo.log.info("failed: " .. table.concat(failed, ","))
"#,
    );
    node.start(false).await;
    node.expect_line("sandbox", "failed: ").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn log_levels_and_print() {
    let mut node = Node::new();
    node.write(
        "talk.lua",
        r#"
bricolo.log.info("count", 3, true)
bricolo.log.warn("careful")
bricolo.log.error("broken")
print("a", 1)
bricolo.log.info(bricolo.script)
"#,
    );
    node.start(false).await;
    node.expect_line("talk", "a\t1").await;
    let lines = node.providers.log.recent(Some("talk.lua"), 10);
    let got: Vec<_> = lines
        .iter()
        .map(|l| (format!("{:?}", l.level), l.message.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("Info".into(), "count 3 true"),
            ("Warn".into(), "careful"),
            ("Error".into(), "broken"),
            ("Info".into(), "a\t1"),
            ("Info".into(), "talk"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn kv_is_private_to_each_script() {
    let mut node = Node::new();
    for name in ["a", "b"] {
        node.write(
            &format!("{name}.lua"),
            &format!(
                r#"
assert(bricolo.kv.set("k", "from {name}"))
bricolo.timer.after(30, function()
  bricolo.log.info(bricolo.kv.get("k"), #bricolo.kv.keys())
end)
"#
            ),
        );
    }
    node.start(false).await;
    node.expect_line("a", "from a 1").await;
    node.expect_line("b", "from b 1").await;
    assert!(node.root.join("data/a.lua/kv.json").is_file());
    assert!(node.root.join("data/b.lua/kv.json").is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn timers_and_sleep_run_as_coroutines() {
    let mut node = Node::new();
    node.write(
        "clock.lua",
        r#"
local t0 = os.clock()
bricolo.timer.sleep(20)
bricolo.log.info("body slept")

local n = 0
local id
id = bricolo.timer.every(10, function(timer)
  n = n + 1
  if n == 3 then
    bricolo.log.info("every fired 3, same id " .. tostring(timer == id))
    bricolo.timer.cancel(id)
  end
end)

bricolo.timer.after(5, function()
  bricolo.log.info("after fired")
  bricolo.timer.sleep(30)
  bricolo.log.info("after slept")
end)

local cancelled = bricolo.timer.after(20, function() bricolo.log.info("should not fire") end)
bricolo.timer.cancel(cancelled)
"#,
    );
    node.start(false).await;
    for line in [
        "body slept",
        "after fired",
        "every fired 3, same id true",
        "after slept",
    ] {
        node.expect_line("clock", line).await;
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    let lines = node.lines("clock");
    assert!(!lines.iter().any(|l| l == "should not fire"), "{lines:?}");
    assert_eq!(node.providers.timer.count("clock.lua"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn async_calls_return_values_or_nil_and_an_error() {
    // tts goes to an offline peer through the router; speech is local.
    let mut node = Node::with_rules("[routes]\ntts = [\"desktop\"]");
    std::fs::write(node.root.join("ok.wav"), "ok").unwrap();
    std::fs::write(node.root.join("bad.wav"), "noise").unwrap();
    let root = node.root.display().to_string();
    node.write(
        "voice.lua",
        &format!(
            r#"
local root = [==[{root}]==]
local text, err = bricolo.speech.transcribe(root .. "/ok.wav")
bricolo.log.info("text", text, err)
text, err = bricolo.speech.transcribe(root .. "/bad.wav")
bricolo.log.info("bad", text, err)
text, err = bricolo.speech.transcribe(root .. "/missing.wav")
bricolo.log.info("missing", text, err:match("^failed: cannot read") ~= nil)
text, err = bricolo.speech.transcribe_audio("ok", "flac")
bricolo.log.info("bytes", text)
local ok
ok, err = bricolo.tts.speak("hi")
bricolo.log.info("speak", ok, err)
"#
        ),
    );
    node.start(false).await;
    node.expect_line("voice", "text hello world (wav) nil")
        .await;
    node.expect_line("voice", "bad nil failed: bad audio").await;
    node.expect_line("voice", "missing nil true").await;
    node.expect_line("voice", "bytes hello world (flac)").await;
    node.expect_line("voice", "speak nil unavailable").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hotkey_speaks_the_clipboard() {
    let mut node = Node::new();
    node.write(
        "speak.lua",
        r#"
assert(bricolo.clipboard.set("read me"))
local id = assert(bricolo.hotkey.bind({"cmd", "alt"}, "s", function(id)
  local text = bricolo.clipboard.get()
  assert(bricolo.tts.speak(text))
  bricolo.log.info("spoke " .. id)
end))
bricolo.log.info("bound " .. id)
"#,
    );
    node.start(false).await;
    node.expect_line("speak", "bound h1").await;
    let bound = node.hotkeys.0.lock().unwrap().clone();
    assert_eq!(
        bound,
        [("speak.lua".into(), "h1".into(), "cmd+alt+s".into())]
    );
    (node.mailbox.sink())("speak.lua", ProviderEvent::Hotkey("h1".into()));
    node.expect_line("speak", "spoke h1").await;
    assert_eq!(*node.tts.0.lock().unwrap(), ["read me"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn hotkey_release_calls_the_second_function() {
    let mut node = Node::new();
    node.write(
        "talk.lua",
        r#"
local held = assert(bricolo.hotkey.bind("fn",
  function(id) bricolo.log.info("down " .. id) end,
  function(id) bricolo.log.info("up " .. id) end))
local tap = assert(bricolo.hotkey.bind({"cmd"}, "t", function() bricolo.log.info("tap") end))
bricolo.log.info("bound " .. held .. " " .. tap)
"#,
    );
    node.start(false).await;
    node.expect_line("talk", "bound h1 h2").await;
    let sink = node.mailbox.sink();
    sink("talk.lua", ProviderEvent::Hotkey("h1".into()));
    node.expect_line("talk", "down h1").await;
    // A release without a release function is dropped.
    sink("talk.lua", ProviderEvent::HotkeyReleased("h2".into()));
    sink("talk.lua", ProviderEvent::HotkeyReleased("h1".into()));
    node.expect_line("talk", "up h1").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_tile_and_panels_call_back_the_script() {
    let mut node = Node::new();
    node.write(
        "dict.lua",
        r#"
assert(bricolo.ui.set_tile{
  title = "Dictation", status = "idle",
  switches = { {id = "review", label = "Review", on = true} },
  buttons = { {id = "go", label = "Go"} },
  on_switch = function(id, on) bricolo.log.info("switch " .. id .. " " .. tostring(on)) end,
  on_button = function(id) bricolo.log.info("button " .. id) end,
})
assert(bricolo.ui.set_widget{title = "Clock", rows = { {label = "Paris", value = "20:05"} }})
local cap = assert(bricolo.ui.caption("hello there"))
local list = assert(bricolo.ui.show_panel{
  kind = "choice_list", title = "Pick", choices = {"one", "two"},
  on_choice = function(i, label) bricolo.log.info("picked " .. tostring(i) .. " " .. tostring(label)) end,
})
local review = assert(bricolo.ui.show_panel{
  kind = "review", text = "draft",
  on_review = function(ok, text) bricolo.log.info("review " .. tostring(ok) .. " " .. text) end,
})
local ok, err = bricolo.ui.show_panel{kind = "choice_list", choices = {}}
bricolo.log.info("empty list: " .. tostring(ok) .. " " .. err)
bricolo.log.info("panels " .. cap .. " " .. list .. " " .. review)
"#,
    );
    node.start(false).await;
    let ui = node.providers.ui.clone();
    let snap = ui.snapshot();
    let tile = &snap.tiles["dict.lua"];
    assert_eq!(tile.status_text, "idle");
    assert!(tile.switches[0].on);
    let widget = &snap.widgets["dict.lua"];
    assert_eq!(widget.title, "Clock");
    assert_eq!(
        (widget.rows[0].label.as_str(), widget.rows[0].value.as_str()),
        ("Paris", "20:05")
    );
    assert_eq!(snap.panels.len(), 3);
    assert_eq!(snap.panels[0].panel.kind, PanelKind::Caption);
    assert_eq!(snap.panels[0].panel.text, "hello there");
    let id = |kind: PanelKind| {
        snap.panels
            .iter()
            .find(|p| p.panel.kind == kind)
            .unwrap()
            .id
            .clone()
    };
    node.expect_line(
        "dict",
        "empty list: nil failed: a choice list needs choices",
    )
    .await;
    ui.act(UiAction::Switch {
        owner: "dict.lua".into(),
        id: "review".into(),
        on: false,
    })
    .unwrap();
    node.expect_line("dict", "switch review false").await;
    ui.act(UiAction::Button {
        owner: "dict.lua".into(),
        id: "go".into(),
    })
    .unwrap();
    node.expect_line("dict", "button go").await;
    ui.act(UiAction::Choice {
        panel: id(PanelKind::ChoiceList),
        index: Some(1),
    })
    .unwrap();
    node.expect_line("dict", "picked 2 two").await;
    ui.act(UiAction::Review {
        panel: id(PanelKind::Review),
        accepted: true,
        text: "final".into(),
    })
    .unwrap();
    node.expect_line("dict", "review true final").await;
    // Stopping the script takes its tile, widget and caption away.
    node.scripts.as_ref().unwrap().shutdown().await;
    let snap = ui.snapshot();
    assert!(
        snap.tiles.is_empty() && snap.widgets.is_empty() && snap.panels.is_empty(),
        "{snap:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_scripts_stay_off_until_enabled() {
    let mut node = Node::new();
    node.write("a.lua", "bricolo.log.info('a up')");
    node.write("b.lua", "bricolo.log.info('b up')");
    node.disabled = ["b".to_string()].into();
    let scripts = node.start(false).await;
    node.expect_line("a", "a up").await;
    assert_eq!(node.info("b").await.unwrap().state, State::Disabled);
    scripts.reload("b").await;
    assert_eq!(node.info("b").await.unwrap().state, State::Disabled);
    assert!(node.lines("b").is_empty());
    scripts.set_enabled("b", true).await.unwrap();
    node.expect_line("b", "b up").await;
    scripts.set_enabled("a", false).await.unwrap();
    assert_eq!(node.info("a").await.unwrap().state, State::Disabled);
    assert_eq!(scripts.disabled(), ["a".to_string()].into());
    assert!(scripts.set_enabled("nope", true).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn disk_reads_writes_and_lists_any_path() {
    let mut node = Node::new();
    let root = node.root.display().to_string();
    node.write(
        "files.lua",
        &format!(
            r#"
local root = [==[{root}]==]
assert(bricolo.disk.write(root .. "/note.txt", "line\0bytes"))
local data = assert(bricolo.disk.read(root .. "/note.txt"))
local names = assert(bricolo.disk.list(root))
bricolo.log.info(#data, table.concat(names, ","))
"#
        ),
    );
    node.start(false).await;
    node.expect_line("files", "10 note.txt,scripts").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_script_restarts_once_then_stays_off_while_others_run() {
    let mut node = Node::new();
    node.write(
        "bad.lua",
        r#"
bricolo.log.info("bad started")
bricolo.timer.after(10, function() error("boom") end)
"#,
    );
    node.write(
        "good.lua",
        r#"
local n = 0
bricolo.timer.every(10, function()
  n = n + 1
  bricolo.log.info("tick " .. n)
end)
"#,
    );
    node.write("broken.lua", "this is not lua");
    node.start(false).await;
    let info = node.wait_state("bad", State::Off).await;
    assert_eq!(info.restarts, 1);
    assert_eq!(
        info.error.as_deref(),
        Some("runtime error: bad.lua:3: boom"),
        "{info:?}"
    );
    let started = node
        .lines("bad")
        .iter()
        .filter(|l| *l == "bad started")
        .count();
    assert_eq!(started, 2);
    assert_eq!(node.providers.timer.count("bad.lua"), 0);

    let broken = node.info("broken").await.unwrap();
    assert_eq!(broken.state, State::Off);
    assert!(
        broken.error.as_deref().unwrap().contains("broken.lua:1"),
        "{broken:?}"
    );

    let good = node.info("good").await.unwrap();
    assert_eq!((good.state, good.restarts), (State::Running, 0));
    let ticks = node.lines("good").len();
    node.expect_line("good", &format!("tick {}", ticks + 2))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_callback_that_spins_past_its_budget_is_stopped() {
    let mut node = Node::new();
    node.write(
        "spin.lua",
        "bricolo.timer.after(1, function() while true do end end)",
    );
    // Catching the budget error does not keep a loop alive.
    node.write(
        "sneaky.lua",
        r#"
bricolo.timer.after(1, function()
  while true do
    pcall(function() while true do end end)
  end
end)
"#,
    );
    let limits = Limits {
        event_budget_ms: 50,
        init_budget_ms: 2000,
    };
    node.start_with(false, limits).await;
    let info = node.wait_state("spin", State::Off).await;
    assert!(
        info.error
            .as_deref()
            .unwrap()
            .contains("ran past its 50 ms budget"),
        "{info:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn init_and_dispose_hooks_run() {
    let mut node = Node::new();
    node.write(
        "hooks.lua",
        r#"
function init() bricolo.log.info("init") end
function dispose() bricolo.log.info("dispose") end
"#,
    );
    let scripts = node.start(false).await;
    node.expect_line("hooks", "init").await;
    scripts.shutdown().await;
    node.expect_line("hooks", "dispose").await;
    assert_eq!(node.info("hooks").await.unwrap().state, State::Stopped);
}

fn count(lines: &[String], line: &str) -> usize {
    lines.iter().filter(|l| *l == line).count()
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_one_script_reloads_only_it_within_a_second() {
    let mut node = Node::new();
    node.write("a.lua", "bricolo.log.info('a v1')");
    node.write("b.lua", "bricolo.log.info('b started')");
    node.start(true).await;
    node.expect_line("a", "a v1").await;
    node.expect_line("b", "b started").await;
    // Let the watcher settle so the first writes do not count.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let saved = Instant::now();
    node.write("a.lua", "bricolo.log.info('a v2')");
    assert!(
        node.wait_line("a", "a v2", Duration::from_secs(1)).await,
        "a did not reload within 1 s: {:?}",
        node.lines("a")
    );
    eprintln!("reloaded in {} ms", saved.elapsed().as_millis());
    assert_eq!(count(&node.lines("b"), "b started"), 1);

    // A file that does not compile leaves the old version running.
    node.write("a.lua", "bricolo.log.info('a v3'");
    let start = Instant::now();
    loop {
        let info = node.info("a").await.unwrap();
        if info
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("reload refused"))
        {
            assert_eq!(info.state, State::Running);
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{info:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A new file starts; a deleted one stops.
    node.write("c.lua", "bricolo.log.info('c started')");
    node.expect_line("c", "c started").await;
    std::fs::remove_file(node.dir.join("c.lua")).unwrap();
    let start = Instant::now();
    while node.info("c").await.is_some() {
        assert!(start.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(count(&node.lines("b"), "b started"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_require_modules_from_lib_and_reload_when_they_change() {
    let mut node = Node::new();
    node.write(
        "lib/greet.lua",
        r#"
local M = {}
function M.hello(name) return "hello " .. name end
return M
"#,
    );
    node.write("lib/text/init.lua", "return { upper = string.upper }");
    node.write(
        "main.lua",
        r#"
local greet = require("lib.greet")
assert(require("lib.greet") == greet)
local text = require("lib.text")
bricolo.log.info(text.upper(greet.hello("lua")))
"#,
    );
    let scripts = node.start(true).await;
    node.expect_line("main", "HELLO LUA").await;
    let names: Vec<String> = scripts
        .status()
        .await
        .into_iter()
        .map(|s| s.info.name)
        .collect();
    assert_eq!(names, ["main"]);
    tokio::time::sleep(Duration::from_millis(300)).await;

    node.write(
        "lib/greet.lua",
        "return { hello = function(name) return 'hi ' .. name end }",
    );
    node.expect_line("main", "HI LUA").await;
}

#[test]
fn starter_names() {
    assert!(bricolo_lua::valid_name("speak_clipboard-2"));
    assert!(!bricolo_lua::valid_name("a.b"));
    assert_eq!(bricolo_lua::caller_name("speak"), "speak.lua");
}

#[tokio::test(flavor = "multi_thread")]
async fn waiting_calls_work_inside_coroutines_the_script_makes() {
    let mut node = Node::new();
    node.write(
        "co.lua",
        r#"
local co = coroutine.create(function(a)
  local b = coroutine.yield(a + 1)
  local slept = bricolo.timer.sleep(10)
  return b * 2, slept
end)
bricolo.log.info("resume", coroutine.resume(co, 1))
bricolo.log.info("resume", coroutine.resume(co, 5))
bricolo.log.info(coroutine.status(co))

local gen = coroutine.wrap(function()
  for i = 1, 2 do
    bricolo.timer.sleep(5)
    coroutine.yield(i)
  end
  return "done"
end)
bricolo.log.info("wrap", gen(), gen(), gen())
local ok, err = pcall(coroutine.wrap(function() error("inner", 0) end))
bricolo.log.info("wrap error", ok, err)
"#,
    );
    node.start(false).await;
    node.expect_line("co", "resume true 2").await;
    node.expect_line("co", "resume true 10 true").await;
    node.expect_line("co", "dead").await;
    node.expect_line("co", "wrap 1 2 done").await;
    node.expect_line("co", "wrap error false inner").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn window_app_and_notify_tables() {
    let mut node = Node::new();
    node.write(
        "desk.lua",
        r#"
local w = assert(bricolo.window.focused())
local s = assert(bricolo.window.screens())[1]
local v = s.visible_frame
assert(bricolo.window.set_frame(w.id, {x = v.x, y = v.y, w = v.w // 2, h = v.h}))
local moved = assert(bricolo.window.list())[1]
bricolo.log.info(w.app, w.title, moved.frame.x, moved.frame.y, moved.frame.w, moved.frame.h, tostring(s.main))
local ok, err = bricolo.window.focus(99)
bricolo.log.info("focus", tostring(ok), err)
local front = assert(bricolo.app.frontmost())
bricolo.log.info(front.name, front.bundle_id, front.pid, #assert(bricolo.app.running()))
assert(bricolo.app.launch("com.apple.Notes"))
assert(bricolo.app.open_url("https://example.com"))
ok, err = bricolo.app.open_url("file:///etc/hosts")
bricolo.log.info("file url", tostring(ok), err)
assert(bricolo.notify.show("Title", "Body"))
assert(bricolo.notify.show("Only a title"))
local st = assert(bricolo.tts.status())
bricolo.log.info("tts", tostring(st.speaking), st.queued, st.voice)
"#,
    );
    node.start(false).await;
    node.expect_line("desk", "Notes todo 0 25 720 875 true")
        .await;
    node.expect_line("desk", "focus nil failed: no such window")
        .await;
    node.expect_line("desk", "Finder com.apple.finder 42 1")
        .await;
    node.expect_line("desk", "file url nil denied").await;
    node.expect_line("desk", "tts false 0 alba").await;
    assert_eq!(
        node.windows.0.lock().unwrap().frame,
        Rect {
            x: 0,
            y: 25,
            width: 720,
            height: 875
        }
    );
    assert_eq!(
        *node.apps.0.lock().unwrap(),
        ["launch com.apple.Notes", "open https://example.com"]
    );
    assert_eq!(
        *node.notes.0.lock().unwrap(),
        [
            ("Title".to_string(), "Body".to_string()),
            ("Only a title".to_string(), String::new())
        ]
    );
}

// /bin/echo and /bin/sh: Windows has neither.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn shell_runs_any_absolute_program_without_an_allow_list() {
    let mut node = Node::new();
    node.write(
        "sh.lua",
        r#"
local out = assert(bricolo.shell.run("/bin/echo", {"hello", "world"}))
bricolo.log.info("echo", out.code, out.stdout:gsub("\n$", ""))
out = assert(bricolo.shell.run("/bin/sh", {"-c", "echo $GREETING; exit 3"}, {GREETING = "hi"}))
bricolo.log.info("sh", out.code, (out.stdout:gsub("\n$", "")))
local ok, err = bricolo.shell.run("echo", {"x"})
bricolo.log.info("relative", tostring(ok), err)
ok, err = bricolo.shell.run("/bin/echo", {}, {PATH = "/tmp"})
bricolo.log.info("path", tostring(ok), err)
"#,
    );
    node.start(false).await;
    node.expect_line("sh", "echo 0 hello world 1").await;
    node.expect_line("sh", "sh 3 hi").await;
    node.expect_line("sh", "relative nil denied").await;
    node.expect_line("sh", "path nil denied").await;
}

/// Answers every request with 201, an `x-seen` header holding the method
/// and path, and the request body echoed back. Returns the port.
fn echo_server() -> u16 {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut parts = line.split_whitespace();
            let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let seen = format!("{method} {path}");
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let head = format!(
                "HTTP/1.1 201 Created\r\nx-seen: {seen}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn http_reaches_any_host_without_an_allow_list() {
    let port = echo_server();
    let mut node = Node::new();
    node.write(
        "web.lua",
        &format!(
            r#"
local base = "http://127.0.0.1:{port}"
local r = assert(bricolo.http.request(base .. "/get"))
bricolo.log.info("get", r.status, r.headers["x-seen"], #r.body)
r = assert(bricolo.http.request{{url = base .. "/post", method = "POST", body = "ping", headers = {{["content-type"] = "text/plain"}}}})
bricolo.log.info("post", r.status, r.headers["x-seen"], r.body)
local ok, err = bricolo.http.request("ftp://127.0.0.1/")
bricolo.log.info("ftp", tostring(ok), err)
"#
        ),
    );
    node.start(false).await;
    node.expect_line("web", "get 201 GET /get 0").await;
    node.expect_line("web", "post 201 POST /post ping").await;
    node.expect_line(
        "web",
        r#"ftp nil failed: "ftp://127.0.0.1/": only http and https are allowed"#,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mouse_moves_clicks_and_scrolls() {
    let mut node = Node::new();
    node.write(
        "point.lua",
        r#"
local p = assert(bricolo.mouse.position())
assert(bricolo.mouse.move_to(100, 200))
local q = assert(bricolo.mouse.position())
assert(bricolo.mouse.click())
assert(bricolo.mouse.click("right", 2))
assert(bricolo.mouse.scroll(0, -3))
local _, err = bricolo.mouse.click("left", 4)
local bad = pcall(bricolo.mouse.click, "thumb")
bricolo.log.info("done", p.x, p.y, q.x, q.y, err, tostring(bad))
"#,
    );
    node.start(false).await;
    node.expect_line(
        "point",
        "done 5 6 100 200 failed: click count must be 1 to 3 false",
    )
    .await;
    assert_eq!(
        *node.mouse.0.lock().unwrap(),
        [
            "move 100 200",
            "click Left 1",
            "click Right 2",
            "scroll 0 -3"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mouse_rounds_fractional_numbers() {
    let mut node = Node::new();
    node.write(
        "round.lua",
        r#"
assert(bricolo.mouse.move_to(100.6, 200.4))
assert(bricolo.mouse.scroll(0.4, -2.6))
local _, err = bricolo.mouse.move_to(1e12, 0)
bricolo.log.info("done", err)
"#,
    );
    node.start(false).await;
    node.expect_line("round", "done failed: coordinate out of range")
        .await;
    assert_eq!(
        *node.mouse.0.lock().unwrap(),
        ["move 101 200", "scroll 0 -3"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn camera_snapshot() {
    let mut node = Node::new();
    node.write(
        "cam.lua",
        r#"bricolo.log.info("cam", assert(bricolo.camera.snapshot()))"#,
    );
    node.start(false).await;
    node.expect_line("cam", "cam /tmp/cam.jpg").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keyboard_mic_and_os() {
    let mut node = Node::new();
    node.write(
        "keys.lua",
        r#"
assert(bricolo.keyboard.keystroke({"cmd", "shift"}, "v"))
assert(bricolo.keyboard.keystroke("ctrl+alt+t"))
assert(bricolo.keyboard.type_text("hi there"))
local bad = pcall(bricolo.keyboard.keystroke, 42)
local id = assert(bricolo.mic.record_start({max_secs = 5}))
local again = assert(bricolo.mic.record_start())
local path = assert(bricolo.mic.record_stop(id))
assert(bricolo.mic.record_cancel(again))
local _, err = bricolo.mic.record_stop(id)
local badfmt = pcall(bricolo.mic.record_start, {format = "mp3"})
bricolo.log.info(bricolo.os(), path, err, tostring(bad), tostring(badfmt))
"#,
    );
    node.start(false).await;
    let os = std::env::consts::OS;
    node.expect_line(
        "keys",
        &format!("{os} /data/keys.lua/recordings/rec-1.wav failed: no recording 1 false false"),
    )
    .await;
    assert_eq!(
        *node.keys.0.lock().unwrap(),
        ["cmd+shift+v", "ctrl+alt+t", "type:hi there"]
    );
    // No options means the longest recording.
    let recs = node.mic.0.lock().unwrap().clone();
    assert_eq!(
        recs,
        [
            ("keys.lua".into(), 5, false),
            ("keys.lua".into(), 600, false)
        ]
    );
}

/// The dictation example, unchanged, against the fakes: one press starts
/// a recording and shows a caption, the next stops it, transcribes the
/// file, copies the text, hides the caption and pastes.
#[tokio::test(flavor = "multi_thread")]
async fn dictation_example_records_transcribes_and_pastes() {
    let mut node = Node::new();
    node.write(
        "dictation.lua",
        include_str!("../../../examples/apps/dictation/dictation.lua"),
    );
    node.start(false).await;
    node.expect_line(
        "dictation",
        "dictation: press ctrl+alt+shift+d to start and stop recording",
    )
    .await;
    let bound = node.hotkeys.0.lock().unwrap().clone();
    assert_eq!(
        bound,
        [(
            "dictation.lua".into(),
            "h1".into(),
            "ctrl+alt+shift+d".into()
        )]
    );
    let press = || (node.mailbox.sink())("dictation.lua", ProviderEvent::Hotkey("h1".into()));

    press();
    let start = Instant::now();
    while node.mic.active().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(5), "never started");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(node.mic.0.lock().unwrap()[0].1, 65);
    let ui = node.providers.ui.clone();
    let caption = || {
        ui.snapshot()
            .panels
            .iter()
            .find(|p| p.owner == "dictation.lua" && p.panel.kind == PanelKind::Caption)
            .map(|p| p.panel.text.clone())
    };
    let start = Instant::now();
    while caption().is_none() {
        assert!(start.elapsed() < Duration::from_secs(5), "no caption");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        caption().as_deref(),
        Some("recording; ctrl+alt+shift+d to stop")
    );
    assert!(node.notes.0.lock().unwrap().is_empty());

    press();
    node.expect_line("dictation", "heard: heard rec-1.wav")
        .await;
    assert!(node.mic.active().is_empty());
    assert_eq!(
        node.clipboard.0.lock().unwrap().as_deref(),
        Some("heard rec-1.wav")
    );
    let paste = if cfg!(target_os = "macos") {
        "cmd+v"
    } else {
        "ctrl+v"
    };
    assert_eq!(*node.keys.0.lock().unwrap(), [paste]);
    // Done: the caption is gone and nothing went wrong.
    assert_eq!(caption(), None);
    assert!(ui.snapshot().panels.is_empty());
}

/// The live check example, unchanged, against the fakes: one press runs
/// every check to its summary line without a Lua error, so the script keeps
/// up with the standard library it checks on real machines.
#[tokio::test(flavor = "multi_thread")]
async fn live_check_example_runs_every_check() {
    let mut node = Node::new();
    node.write(
        "live_check.lua",
        include_str!("../../../examples/apps/live-check/live_check.lua"),
    );
    node.start(false).await;
    node.expect_line(
        "live_check",
        "live_check: press ctrl+alt+cmd+shift+l to run the checks",
    )
    .await;
    (node.mailbox.sink())("live_check.lua", ProviderEvent::Hotkey("h1".into()));
    let start = Instant::now();
    let summary = loop {
        let lines = node.lines("live_check");
        if let Some(l) = lines.iter().find(|l| l.starts_with("live check: ")) {
            break l.clone();
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "no summary; it logged {lines:?}; status {:?}",
            node.info("live_check").await,
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let checks: Vec<_> = node
        .lines("live_check")
        .into_iter()
        .filter(|l| l.starts_with("live_check: ") && !l.contains("press "))
        .collect();
    assert!(checks.len() >= 18, "{checks:?}");
    for l in &checks {
        let kind = l["live_check: ".len()..].split(' ').next().unwrap();
        assert!(
            ["PASS", "FAIL", "UNAVAILABLE", "DENIED"].contains(&kind),
            "{l}"
        );
    }
    assert!(summary.contains(" pass, "), "{summary}");
    assert_eq!(node.info("live_check").await.unwrap().error, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_plays_changes_volume_and_stops_on_reload() {
    let mut node = Node::new();
    std::fs::write(node.root.join("noise.wav"), "RIFF").unwrap();
    let noise = node.root.join("noise.wav").to_string_lossy().into_owned();
    let source = |extra: &str| {
        format!(
            r#"
local id = assert(bricolo.audio.play([==[{noise}]==], {{volume = 40, looping = true}}))
assert(bricolo.audio.set_volume(id, 250))
local s = bricolo.audio.sounds()[1]
local _, missing = bricolo.audio.play([==[{noise}.gone]==])
local bad = pcall(bricolo.audio.play, [==[{noise}]==], 7)
bricolo.log.info("{extra}", s.id == id, s.volume, s.looping, missing, tostring(bad))
"#
        )
    };
    node.write("noise.lua", &source("first"));
    node.start(true).await;
    let line = |n: &str| format!("{n} true 100 true failed: no file {noise}.gone false");
    node.expect_line("noise", &line("first")).await;
    assert_eq!(*node.speakers.lock().unwrap(), ["noise.wav 100"]);

    // A reload stops the old sound before the new script plays its own.
    node.write("noise.lua", &source("second"));
    node.expect_line("noise", &line("second")).await;
    assert_eq!(*node.speakers.lock().unwrap(), ["noise.wav 100"]);

    node.write(
        "noise.lua",
        r#"
local ids = {}
for _, s in ipairs(bricolo.audio.sounds()) do ids[#ids + 1] = s.id end
local id = assert(bricolo.audio.play([==[NOISE]==], {volume = -5}))
assert(bricolo.audio.stop(id))
assert(bricolo.audio.stop(id))
bricolo.log.info("third", #ids, #bricolo.audio.sounds())
"#
        .replace("NOISE", &noise)
        .as_str(),
    );
    node.expect_line("noise", "third 0 0").await;
    assert!(node.speakers.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn app_and_system_events_reach_watchers() {
    let mut node = Node::new();
    node.write(
        "watcher.lua",
        r#"
assert(bricolo.app.watch(function(e)
  bricolo.log.info("app", e.change, e.name, e.bundle_id, e.pid)
end))
assert(bricolo.system.watch(function(e) bricolo.log.info("system", e) end))
bricolo.log.info("ready")
"#,
    );
    node.start(false).await;
    node.expect_line("watcher", "ready").await;
    assert!(
        node.apps
            .0
            .lock()
            .unwrap()
            .contains(&"watch watcher.lua true".to_string())
    );
    assert!(
        node.system
            .0
            .lock()
            .unwrap()
            .contains(&"watch watcher.lua true".to_string())
    );
    let sink = node.mailbox.sink();
    sink(
        "watcher.lua",
        ProviderEvent::App(AppEvent {
            change: AppChange::Activated,
            app: AppInfo {
                name: "Zoom".into(),
                bundle_id: Some("us.zoom.xos".into()),
                pid: 9,
            },
        }),
    );
    node.expect_line("watcher", "app activated Zoom us.zoom.xos 9")
        .await;
    sink("watcher.lua", ProviderEvent::System(SystemEvent::DidWake));
    node.expect_line("watcher", "system did_wake").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn system_state_and_keep_awake_until_reload() {
    let mut node = Node::new();
    node.write(
        "awake.lua",
        r#"
local p = assert(bricolo.system.power())
assert(bricolo.system.keep_awake(true))
bricolo.log.info("state", bricolo.system.idle_seconds(), p.battery, p.plugged_in)
"#,
    );
    node.start(true).await;
    node.expect_line("awake", "state 42 87 true").await;
    node.write("awake.lua", r#"bricolo.log.info("again")"#);
    node.expect_line("awake", "again").await;
    let calls = node.system.0.lock().unwrap().clone();
    assert!(
        calls.contains(&"awake awake.lua true".to_string()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"release awake.lua".to_string()),
        "{calls:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn files_watch_reports_created_and_removed() {
    let mut node = Node::new();
    let inbox = node.root.join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    node.write(
        "inbox.lua",
        &format!(
            r#"
local id
id = assert(bricolo.files.watch([==[{}]==], function(e)
  if e.path:match("note%.txt$") then
    bricolo.log.info("file", e.change, e.watch == id)
  end
end))
bricolo.log.info("watching")
"#,
            inbox.display()
        ),
    );
    node.start(false).await;
    node.expect_line("inbox", "watching").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(inbox.join("note.txt"), "hi").unwrap();
    let seen = node
        .wait_line("inbox", "file created true", Duration::from_secs(10))
        .await
        || node
            .wait_line("inbox", "file modified true", Duration::from_secs(1))
            .await;
    assert!(seen, "{:?}", node.lines("inbox"));
    std::fs::remove_file(inbox.join("note.txt")).unwrap();
    assert!(
        node.wait_line("inbox", "file removed true", Duration::from_secs(10))
            .await,
        "{:?}",
        node.lines("inbox")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn screen_capture_and_selected_text() {
    let mut node = Node::new();
    node.write(
        "look.lua",
        r#"
local all = assert(bricolo.screen.capture())
local part = assert(bricolo.screen.capture({x = 10, y = 20, w = 300, h = 200}))
local _, err = bricolo.screen.capture({x = 0, y = 0, w = 0, h = 10})
bricolo.log.info("done", all, part, err, bricolo.selection.get_text())
"#,
    );
    node.start(false).await;
    let shot = shot_path();
    node.expect_line(
        "look",
        &format!("done {shot} {shot} failed: the area is empty hello"),
    )
    .await;
    assert_eq!(
        *node.screen.0.lock().unwrap(),
        [
            None,
            Some(Rect {
                x: 10,
                y: 20,
                width: 300,
                height: 200
            })
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_set_get_and_delete() {
    let mut node = Node::new();
    node.write(
        "keys.lua",
        r#"
assert(bricolo.secrets.set("token", "s3cret"))
local v = bricolo.secrets.get("token")
local none = bricolo.secrets.get("other")
assert(bricolo.secrets.delete("token"))
local gone = bricolo.secrets.get("token")
local _, err = bricolo.secrets.set("bad name", "x")
bricolo.log.info("done", v, tostring(none), tostring(gone), err)
"#,
    );
    node.start(false).await;
    node.expect_line("keys", "done s3cret nil nil failed: bad secret name")
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_code_sessions_are_tables() {
    let mut node = Node::new();
    node.write(
        "cc.lua",
        r#"
local s = assert(bricolo.claude_code.start("/w/repo", "fix it"))
local list = assert(bricolo.claude_code.list())
local st = assert(bricolo.claude_code.status("s1"))
local _, err = bricolo.claude_code.send("s1", "more")
assert(bricolo.claude_code.stop("s1"))
bricolo.log.info("done", s.id, s.dir, s.state, s.title, s.started, #list, list[1].state,
  st.state, st.last_result, tostring(s.last_result), err)
"#,
    );
    node.start(false).await;
    node.expect_line(
        "cc",
        "done s1 /w/repo working fix it 10 1 finished waiting need Bash nil failed: s1 is working",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn window_close_minimize_and_app_quit_hide() {
    let mut node = Node::new();
    node.write(
        "tidy.lua",
        r#"
assert(bricolo.window.set_minimized(7, true))
assert(bricolo.window.set_minimized(7, false))
assert(bricolo.window.close(7))
local _, missing = bricolo.window.close(8)
assert(bricolo.app.set_hidden("Notes", true))
assert(bricolo.app.quit("com.apple.Notes"))
local bad = pcall(bricolo.window.close, "seven")
bricolo.log.info("done", missing, tostring(bad))
"#,
    );
    node.start(false).await;
    node.expect_line("tidy", "done failed: no such window false")
        .await;
    assert_eq!(
        *node.windows.1.lock().unwrap(),
        ["minimized 7 true", "minimized 7 false", "close 7"]
    );
    assert_eq!(
        *node.apps.0.lock().unwrap(),
        ["hidden Notes true", "quit com.apple.Notes"]
    );
}

/// A desktop with one app, "Notes", whose window holds a text field and a
/// Save button that sets the status text.
fn fake_accessibility() -> bricolo_providers::accessibility::Accessibility<
    bricolo_providers::accessibility::fake::FakeBackend,
> {
    use bricolo_providers::accessibility::fake::{FakeBackend, FakeDesktop};
    use bricolo_providers::accessibility::{Accessibility, AxApp, BLOCKED_MACOS, Kind};
    let mut d = FakeDesktop::default();
    let win = d.add(Kind::Window, "window", Some("Notes"), None);
    let field = d.add(Kind::TextField, "text field", Some("Title"), Some(win));
    d.nodes[field].info.value = Some(String::new());
    let status = d.add(Kind::StaticText, "text", None, Some(win));
    d.nodes[status].info.value = Some("Unsaved".into());
    let save = d.add(Kind::Button, "button", Some("Save"), Some(win));
    d.on_press(save, move |d, _| {
        d.nodes[status].info.value = Some("Saved".into())
    });
    d.apps.push((
        AxApp {
            name: "Notes".into(),
            bundle_id: Some("com.example.notes".into()),
            pid: 4242,
            frontmost: true,
        },
        vec![win],
        None,
    ));
    Accessibility::with_blocked(
        FakeBackend(Arc::new(std::sync::Mutex::new(d))),
        None,
        BLOCKED_MACOS,
    )
}

fn fake_runner() -> bricolo_providers::cmd::FakeRunner {
    let r = bricolo_providers::cmd::FakeRunner::default();
    r.answer("/usr/bin/osascript -l AppleScript", "42")
        .answer("wpctl get-volume", "Volume: 0.30")
        .answer("pwsh", "42");
    r
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_drive_an_app_through_accessibility() {
    let mut node = Node::new();
    node.write(
        "notes_bot.lua",
        r#"
local s = assert(bricolo.accessibility.snapshot("com.example.notes"))
local function index(text)
  for line in s.tree:gmatch("[^\n]+") do
    if line:find(text, 1, true) then return tonumber(line:match("^%s*(%d+)")) end
  end
end
local r = assert(bricolo.accessibility.set_value("Notes", index("Title"), "groceries"))
local c = assert(bricolo.accessibility.click("Notes", index("Save")))
local found = assert(bricolo.accessibility.wait("Notes", "Saved", 2000))
local _, blocked = bricolo.accessibility.press_key("Notes", "cmd+q")
local vol = assert(bricolo.device.volume())
local answer = assert(bricolo.scripting.run_applescript("return 6 * 7"))
local procs = assert(bricolo.process.list())
bricolo.log.info("done", r.verification, c.verification, found[1].value, blocked:match("system_shortcut_blocked") ~= nil, vol, answer, #procs > 0)
"#,
    );
    node.start(false).await;
    node.expect_line(
        "notes_bot",
        "done verified unverified Saved true 30 42 true",
    )
    .await;
}
