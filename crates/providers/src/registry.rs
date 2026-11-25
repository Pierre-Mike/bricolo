use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::accessibility::AccessibilityProvider;
use crate::app::AppProvider;
use crate::audio::AudioProvider;
use crate::camera::{CameraConfig, CameraProvider, FfmpegCamera};
use crate::claude_code::{ClaudeCodeConfig, ClaudeCodeProvider, LocalClaudeCode};
use crate::clipboard::ClipboardProvider;
use crate::desktop::Desktop;
use crate::device::DeviceProvider;
use crate::disk::{DiskProvider, LocalDisk};
use crate::echo::EchoReference;
use crate::event::EventSink;
use crate::files::{FilesProvider, NotifyFiles};
use crate::hotkey::{HotkeyProvider, Remap};
use crate::http::{HttpProvider, LocalHttp};
use crate::keyboard::KeyboardProvider;
use crate::kv::{FileKv, KvProvider};
use crate::log::{LogProvider, TracingLog};
use crate::mic::{FfmpegMic, MicConfig, MicProvider};
use crate::mouse::MouseProvider;
use crate::notify::NotifyProvider;
use crate::process::ProcessProvider;
use crate::screen::ScreenProvider;
use crate::scripting::ScriptingProvider;
use crate::secrets::SecretsProvider;
use crate::selection::SelectionProvider;
use crate::sensors::Sensors;
use crate::shell::{LocalShell, ShellProvider};
use crate::sidecar::{SidecarConfig, Sidecars};
use crate::speech::{HttpSpeech, NoSpeech, SpeechConfig, SpeechProvider};
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
use crate::system::NoSystem;
use crate::system::SystemProvider;
use crate::timer::{LocalTimers, TimerProvider};
use crate::tts::{Engine, NoTts, QueueTts, TtsConfig, TtsProvider};
use crate::ui::{LocalUi, UiProvider};
use crate::virtual_screen::VirtualScreenProvider;
use crate::window::WindowProvider;

/// Every capability, named like its WIT interface.
pub const CAPABILITIES: [&str; 30] = [
    "log",
    "timer",
    "hotkey",
    "tts",
    "speech",
    "disk",
    "clipboard",
    "kv",
    "window",
    "app",
    "notify",
    "shell",
    "http",
    "ui",
    "keyboard",
    "mic",
    "audio",
    "system",
    "files",
    "mouse",
    "screen",
    "selection",
    "camera",
    "secrets",
    "claude-code",
    "accessibility",
    "process",
    "device",
    "scripting",
    "virtual-screen",
];

/// Capabilities the router may send to another node. The rest always run
/// on this node: they act on this device's screen, keyboard, mouse, microphone and
/// speakers (`ui` included), or are scoped by allow lists in the plugin's manifest.
pub const ROUTABLE: [&str; 6] = [
    "tts",
    "speech",
    "disk",
    "clipboard",
    "notify",
    "claude-code",
];

/// One provider per capability, and the sidecars behind some of them.
#[derive(Clone)]
pub struct Providers {
    pub log: Arc<dyn LogProvider>,
    pub timer: Arc<dyn TimerProvider>,
    pub kv: Arc<dyn KvProvider>,
    pub disk: Arc<dyn DiskProvider>,
    pub tts: Arc<dyn TtsProvider>,
    pub speech: Arc<dyn SpeechProvider>,
    pub hotkey: Arc<dyn HotkeyProvider>,
    pub clipboard: Arc<dyn ClipboardProvider>,
    pub window: Arc<dyn WindowProvider>,
    pub app: Arc<dyn AppProvider>,
    pub notify: Arc<dyn NotifyProvider>,
    pub shell: Arc<dyn ShellProvider>,
    pub http: Arc<dyn HttpProvider>,
    pub ui: Arc<dyn UiProvider>,
    pub keyboard: Arc<dyn KeyboardProvider>,
    pub mic: Arc<dyn MicProvider>,
    pub audio: Arc<dyn AudioProvider>,
    pub system: Arc<dyn SystemProvider>,
    pub files: Arc<dyn FilesProvider>,
    pub mouse: Arc<dyn MouseProvider>,
    pub screen: Arc<dyn ScreenProvider>,
    pub selection: Arc<dyn SelectionProvider>,
    pub camera: Arc<dyn CameraProvider>,
    pub secrets: Arc<dyn SecretsProvider>,
    pub claude_code: Arc<dyn ClaudeCodeProvider>,
    pub accessibility: Arc<dyn AccessibilityProvider>,
    pub process: Arc<dyn ProcessProvider>,
    pub device: Arc<dyn DeviceProvider>,
    pub scripting: Arc<dyn ScriptingProvider>,
    pub virtual_screen: Arc<dyn VirtualScreenProvider>,
    pub sidecars: Arc<Sidecars>,
    /// Who uses the microphone, camera or screen right now.
    pub sensors: Arc<Sensors>,
}

#[derive(Clone, Debug, Default)]
pub struct LocalOptions {
    /// Holds `<plugin>/kv.json` for each plugin.
    pub data_dir: PathBuf,
    /// Where sidecar output goes; `None` discards it.
    pub logs_dir: Option<PathBuf>,
    /// `[sidecars.<name>]` from `node.toml`.
    pub sidecars: BTreeMap<String, SidecarConfig>,
    pub tts: TtsConfig,
    pub speech: SpeechConfig,
    /// `[mic]` from `node.toml`.
    pub mic: MicConfig,
    /// `[camera]` from `node.toml`.
    pub camera: CameraConfig,
    /// `[claude-code]` from `node.toml`.
    pub claude_code: ClaudeCodeConfig,
    /// The main-thread loop, when the node runs one (`bricolo run` on macOS).
    pub desktop: Option<Desktop>,
    /// Where the chords the user changed in the hub are kept
    /// (`<data>/hotkeys.toml`); `None` keeps them in memory only.
    pub hotkeys_file: Option<PathBuf>,
}

impl LocalOptions {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            ..Self::default()
        }
    }

    /// The base URL a `[tts]` or `[speech]` section points at: its `url`,
    /// else its sidecar's port on 127.0.0.1.
    fn server(&self, url: &Option<String>, sidecar: &Option<String>) -> Option<String> {
        url.clone().or_else(|| {
            let name = sidecar.as_ref()?;
            let config = self.sidecars.get(name);
            if config.is_none() {
                tracing::error!("node.toml names sidecar {name:?}, which is not configured");
            }
            config.map(SidecarConfig::base_url)
        })
    }
}

impl Providers {
    /// The providers of this node. Starts the sidecars. Must be called
    /// inside a Tokio runtime.
    pub fn local(options: LocalOptions, sink: EventSink) -> Self {
        let sidecars = Arc::new(Sidecars::start(
            options.sidecars.clone(),
            options.logs_dir.clone(),
        ));
        // What the speech queue plays, for the microphone to subtract.
        let echo = options
            .mic
            .echo_cancel
            .unwrap_or(true)
            .then(|| Arc::new(EchoReference::default()));
        let tts: Arc<dyn TtsProvider> = match options.server(&options.tts.url, &options.tts.sidecar)
        {
            Some(url) => {
                let player = options.tts.player.clone().unwrap_or_else(default_player);
                Arc::new(QueueTts::with_echo(
                    Engine::Http { url, player },
                    &options.tts,
                    echo.clone(),
                ))
            }
            None => match local_engine() {
                Some(engine) => Arc::new(QueueTts::new(engine, &options.tts)),
                None => Arc::new(NoTts),
            },
        };
        let speech: Arc<dyn SpeechProvider> =
            match options.server(&options.speech.url, &options.speech.sidecar) {
                Some(url) => Arc::new(HttpSpeech::new(
                    url,
                    std::env::temp_dir().join("bricolo-speech"),
                )),
                None => Arc::new(NoSpeech),
            };
        let sink_for_files = sink.clone();
        let has_desktop = options.desktop.is_some();
        let sensors: Arc<Sensors> = Arc::default();
        let desktop = platform(options.desktop, &sink, &options.data_dir, &sensors);
        let ui = Arc::new(LocalUi::new(sink.clone()));
        let mut mic = FfmpegMic::new(options.data_dir.clone(), &options.mic, sensors.clone());
        if let Some(echo) = echo {
            mic = mic.with_echo(echo);
        }
        let mic = Arc::new(mic);
        let camera = Arc::new(FfmpegCamera::new(
            options.data_dir.clone(),
            &options.camera,
            sensors.clone(),
        ));
        let clipboard = rich_clipboard(desktop.clipboard, &options.data_dir);
        Self {
            log: Arc::new(TracingLog::default()),
            timer: Arc::new(LocalTimers::new(sink)),
            kv: Arc::new(FileKv::new(options.data_dir)),
            disk: Arc::new(LocalDisk),
            tts,
            speech,
            hotkey: Arc::new(Remap::new(desktop.hotkey, options.hotkeys_file)),
            clipboard,
            window: desktop.window,
            app: desktop.app,
            notify: desktop.notify,
            shell: Arc::new(LocalShell::default()),
            http: Arc::new(LocalHttp),
            ui,
            keyboard: desktop.keyboard,
            mic,
            audio: audio(),
            system: desktop.system,
            files: Arc::new(NotifyFiles::new(sink_for_files)),
            mouse: desktop.mouse,
            screen: desktop.screen,
            selection: desktop.selection,
            camera,
            secrets: secrets(has_desktop),
            claude_code: Arc::new(LocalClaudeCode::from_config(&options.claude_code)),
            accessibility: desktop.accessibility,
            process: crate::process::local(),
            device: desktop.device,
            scripting: crate::scripting::local(std::env::temp_dir().join("bricolo-scripting")),
            virtual_screen: desktop.virtual_screen,
            sidecars,
            sensors,
        }
    }

    /// Drops what `plugin` holds: timers, hotkeys, recordings, sounds, its
    /// tile and panels, and its sensors. Called when a plugin stops, restarts or
    /// reloads.
    pub fn release(&self, plugin: &str) {
        self.timer.cancel_owned(plugin);
        self.hotkey.release(plugin);
        self.mic.release(plugin);
        self.audio.release(plugin);
        self.app.release(plugin);
        self.system.release(plugin);
        self.files.release(plugin);
        self.ui.release(plugin);
        self.sensors.release(plugin);
        self.accessibility.release(plugin);
        self.virtual_screen.release(plugin);
    }

    /// Stops the recordings, the sounds and the sidecars. Called once the
    /// plugins have stopped.
    pub async fn shutdown(&self) {
        self.mic.shutdown().await;
        self.audio.shutdown().await;
        self.sidecars.shutdown().await;
    }
}

/// The WAV player when `[tts] player` is not set.
#[cfg(target_os = "macos")]
fn default_player() -> Vec<String> {
    vec!["/usr/bin/afplay".into()]
}

#[cfg(target_os = "linux")]
fn default_player() -> Vec<String> {
    crate::linux::tts::player()
}

/// `ffplay` if installed, else PowerShell's `SoundPlayer`.
#[cfg(target_os = "windows")]
fn default_player() -> Vec<String> {
    crate::windows::tts::player()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn default_player() -> Vec<String> {
    vec!["aplay".into()]
}

/// The Keychain on macOS, and only with the main-thread loop (a node in a
/// GUI session); the Secret Service on Linux, checked on each call; the
/// Credential Manager on Windows; `unavailable` elsewhere.
#[cfg(target_os = "macos")]
fn secrets(has_desktop: bool) -> Arc<dyn SecretsProvider> {
    use crate::secrets::Secrets;
    if has_desktop {
        Arc::new(Secrets::new(crate::macos::keychain::KeychainStore))
    } else {
        Arc::new(crate::secrets::NoSecrets)
    }
}

#[cfg(target_os = "linux")]
fn secrets(_: bool) -> Arc<dyn SecretsProvider> {
    Arc::new(crate::secrets::Secrets::new(
        crate::linux::secret_service::SecretServiceStore,
    ))
}

#[cfg(target_os = "windows")]
fn secrets(_: bool) -> Arc<dyn SecretsProvider> {
    Arc::new(crate::secrets::Secrets::new(
        crate::windows::credentials::CredentialStore,
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn secrets(_: bool) -> Arc<dyn SecretsProvider> {
    Arc::new(crate::secrets::NoSecrets)
}

/// AVAudioPlayer on macOS; ffmpeg elsewhere.
fn audio() -> Arc<dyn AudioProvider> {
    use crate::audio::Player;
    #[cfg(target_os = "macos")]
    let backend = Arc::new(crate::macos::audio::AvPlayer::new());
    #[cfg(not(target_os = "macos"))]
    let backend = Arc::new(crate::audio::FfmpegBackend::new(
        None,
        crate::audio::default_outputs(),
    ));
    Arc::new(Player::new(backend))
}

/// The speech engine when `node.toml` names no TTS server.
#[cfg(target_os = "macos")]
fn local_engine() -> Option<Engine> {
    Some(Engine::Say {
        program: "/usr/bin/say".into(),
    })
}

/// espeak-ng, espeak or spd-say, the first one installed.
#[cfg(target_os = "linux")]
fn local_engine() -> Option<Engine> {
    crate::linux::tts::engine()
}

/// SAPI 5.
#[cfg(target_os = "windows")]
fn local_engine() -> Option<Engine> {
    crate::windows::tts::engine()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn local_engine() -> Option<Engine> {
    None
}

/// The providers that act on this device's desktop.
struct Platform {
    hotkey: Arc<dyn HotkeyProvider>,
    clipboard: Arc<dyn ClipboardProvider>,
    window: Arc<dyn WindowProvider>,
    app: Arc<dyn AppProvider>,
    notify: Arc<dyn NotifyProvider>,
    keyboard: Arc<dyn KeyboardProvider>,
    system: Arc<dyn SystemProvider>,
    mouse: Arc<dyn MouseProvider>,
    screen: Arc<dyn ScreenProvider>,
    selection: Arc<dyn SelectionProvider>,
    accessibility: Arc<dyn AccessibilityProvider>,
    device: Arc<dyn DeviceProvider>,
    virtual_screen: Arc<dyn VirtualScreenProvider>,
}

/// Hotkeys and screens need the main-thread loop; the rest work without it.
#[cfg(target_os = "macos")]
fn platform(
    desktop: Option<Desktop>,
    sink: &EventSink,
    data_dir: &std::path::Path,
    sensors: &Arc<Sensors>,
) -> Platform {
    use crate::hotkey::NoHotkeys;
    use crate::macos;
    use crate::mouse::NoMouse;
    use crate::screen::NoScreen;
    use crate::selection::NoSelection;
    let main = desktop.map(|d| d.main);
    let has_loop = main.is_some();
    let screen: Arc<dyn ScreenProvider> = match &main {
        Some(_) => Arc::new(macos::screen::Screencapture::new(
            data_dir.to_path_buf(),
            sensors.clone(),
        )),
        None => Arc::new(NoScreen),
    };
    Platform {
        hotkey: match &main {
            Some(main) => Arc::new(macos::hotkey::MacHotkeys::new(main.clone(), sink.clone())),
            None => Arc::new(NoHotkeys),
        },
        clipboard: Arc::new(macos::clipboard::Pasteboard),
        window: Arc::new(macos::window::MacWindows::new(main.clone())),
        app: Arc::new(macos::app::MacApps::new(has_loop.then(|| sink.clone()))),
        notify: Arc::new(macos::notify::Osascript),
        keyboard: Arc::new(macos::keyboard::MacKeyboard),
        system: if has_loop {
            Arc::new(macos::system::MacSystem::new(sink.clone()))
        } else {
            Arc::new(NoSystem)
        },
        // Like `window`, the pointer is only driven by a node with the
        // main-thread loop.
        mouse: if has_loop {
            Arc::new(macos::mouse::MacMouse)
        } else {
            Arc::new(NoMouse)
        },
        // Screen Recording and Accessibility answers need the desktop session.
        screen: screen.clone(),
        selection: match &main {
            Some(_) => Arc::new(macos::selection::MacSelection),
            None => Arc::new(NoSelection),
        },
        // AXUIElement calls work from any thread; screenshots need the loop.
        accessibility: Arc::new(crate::accessibility::Accessibility::new(
            macos::accessibility::MacAx,
            has_loop.then_some(screen),
        )),
        device: Arc::new(crate::device::Device::new(
            crate::device::Os::MacOs,
            crate::cmd::SystemRunner,
            Box::new(macos::device::MacNative),
            crate::disk::home(),
        )),
        // No public API for a display no monitor shows; see virtual_screen.rs.
        virtual_screen: Arc::new(crate::virtual_screen::NoVirtualScreen),
    }
}

/// X11 for windows, hotkeys, the app list and the keyboard, each answering
/// `unavailable` on Wayland or without a display; arboard for the
/// clipboard; D-Bus for notifications. No main-thread loop.
#[cfg(target_os = "linux")]
fn platform(
    _: Option<Desktop>,
    sink: &EventSink,
    data_dir: &std::path::Path,
    sensors: &Arc<Sensors>,
) -> Platform {
    use crate::linux;
    let clipboard = Arc::new(linux::clipboard::ArboardClipboard::new());
    let x11 = Arc::new(linux::x11::X11::default());
    let screen: Arc<dyn ScreenProvider> = Arc::new(linux::screen::LinuxScreen::new(
        data_dir.to_path_buf(),
        sensors.clone(),
    ));
    Platform {
        hotkey: Arc::new(linux::desktop::LinuxHotkeys::new(sink.clone())),
        clipboard: clipboard.clone(),
        window: Arc::new(linux::desktop::LinuxWindows::new(x11.clone())),
        app: Arc::new(linux::app::LinuxApps::new(x11)),
        notify: Arc::new(linux::notify::DbusNotify::new()),
        keyboard: Arc::new(linux::desktop::LinuxKeyboard::default()),
        system: Arc::new(linux::system::LinuxSystem::new(sink.clone())),
        mouse: Arc::new(linux::mouse::X11Mouse::default()),
        screen: screen.clone(),
        selection: Arc::new(linux::clipboard::PrimarySelection(clipboard)),
        // AT-SPI reads and acts in the background; pictures of a window come
        // from the screen provider.
        accessibility: Arc::new(crate::accessibility::Accessibility::new(
            linux::accessibility::AtSpi::new(),
            Some(screen),
        )),
        device: Arc::new(crate::device::Device::new(
            crate::device::Os::Linux,
            crate::cmd::SystemRunner,
            Box::new(crate::device::NoNative),
            crate::disk::home(),
        )),
        // One Xvfb for each screen.
        virtual_screen: Arc::new(crate::virtual_screen::VirtualScreens::new(
            linux::virtual_screen::Xvfb,
            data_dir.to_path_buf(),
        )),
    }
}

/// Windows: Win32 through the `windows` crate (`windows/`). Each line
/// still answering `No*` is a capability Windows has no backend for yet.
#[cfg(target_os = "windows")]
fn platform(
    _: Option<Desktop>,
    sink: &EventSink,
    data_dir: &std::path::Path,
    sensors: &Arc<Sensors>,
) -> Platform {
    use crate::selection::NoSelection;
    #[allow(unused_imports)]
    use crate::windows;
    let screen: Arc<dyn crate::screen::ScreenProvider> = Arc::new(windows::screen::GdiScreen::new(
        data_dir.to_path_buf(),
        sensors.clone(),
    ));
    Platform {
        hotkey: Arc::new(windows::hotkey::WinHotkeys::new(sink.clone())),
        clipboard: Arc::new(windows::clipboard::WinClipboard),
        window: Arc::new(windows::window::WinWindows::new()),
        app: Arc::new(windows::app::WinApps::new(sink.clone())),
        notify: Arc::new(windows::notify::ToastNotify::new()),
        keyboard: Arc::new(windows::keyboard::WinKeyboard),
        system: Arc::new(windows::system::WinSystem::new(sink.clone())),
        mouse: Arc::new(windows::mouse::WinMouse::new()),
        screen: screen.clone(),
        selection: Arc::new(NoSelection),
        accessibility: Arc::new(crate::accessibility::Accessibility::new(
            windows::accessibility::Uia::new(),
            Some(screen),
        )),
        device: Arc::new(crate::device::Device::new(
            crate::device::Os::Windows,
            crate::cmd::SystemRunner,
            Box::new(crate::device::NoNative),
            crate::disk::home(),
        )),
        virtual_screen: Arc::new(crate::virtual_screen::VirtualScreens::new(
            windows::virtual_screen::WindowsScreens::default(),
            data_dir.to_path_buf(),
        )),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform(_: Option<Desktop>, _: &EventSink, _: &std::path::Path, _: &Arc<Sensors>) -> Platform {
    use crate::app::NoApps;
    use crate::clipboard::NoClipboard;
    use crate::hotkey::NoHotkeys;
    use crate::keyboard::NoKeyboard;
    use crate::mouse::NoMouse;
    use crate::notify::NoNotify;
    use crate::screen::NoScreen;
    use crate::selection::NoSelection;
    use crate::window::NoWindows;
    Platform {
        hotkey: Arc::new(NoHotkeys),
        clipboard: Arc::new(NoClipboard),
        window: Arc::new(NoWindows),
        app: Arc::new(NoApps),
        notify: Arc::new(NoNotify),
        keyboard: Arc::new(NoKeyboard),
        system: Arc::new(NoSystem),
        mouse: Arc::new(NoMouse),
        screen: Arc::new(NoScreen),
        selection: Arc::new(NoSelection),
        accessibility: Arc::new(crate::accessibility::NoAccessibility),
        device: Arc::new(crate::device::NoDevice),
        virtual_screen: Arc::new(crate::virtual_screen::NoVirtualScreen),
    }
}

/// Adds images and copied files to the system's text clipboard.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn rich_clipboard(
    text: Arc<dyn ClipboardProvider>,
    data_dir: &std::path::Path,
) -> Arc<dyn ClipboardProvider> {
    Arc::new(crate::clipboard::Rich::new(
        text,
        Arc::new(crate::clipboard::ArboardData::default()),
        data_dir.to_path_buf(),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn rich_clipboard(
    text: Arc<dyn ClipboardProvider>,
    _: &std::path::Path,
) -> Arc<dyn ClipboardProvider> {
    text
}
