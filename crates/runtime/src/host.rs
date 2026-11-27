//! The Wasmtime side of the standard library: one `HostState` per plugin
//! store, and the import implementations that forward to the providers.

use std::path::Path;

use bricolo_providers::log::Level as LogLevel;
use bricolo_providers::{
    AppChange, CallError as ProviderError, Caller, FileChange, ProviderEvent, Providers,
    SystemEvent,
};
use wasmtime::AsContextMut;
use wasmtime::component::{Accessor, HasSelf, ResourceTable};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "bricolo:stdlib/plugin",
});

pub use bricolo::stdlib::types::CallError;
use bricolo::stdlib::{accessibility, device, process, scripting, virtual_screen};
use bricolo::stdlib::{
    app, audio, camera, claude_code, clipboard, disk, hotkey, http, keyboard, kv, log, mic, mouse,
    notify, screen, secrets, selection, shell, speech, timer, tts, types, ui, window,
};
use bricolo::stdlib::{files, system};
use bricolo_providers::accessibility as pax;
use bricolo_providers::app::AppInfo;
use bricolo_providers::audio::{PlayOptions, Sound};
use bricolo_providers::claude_code::{Session as CcSession, SessionState as CcState};
use bricolo_providers::http::{Request as HttpRequest, Response as HttpResponse};
use bricolo_providers::mic::{AudioFormat, RecordOptions};
use bricolo_providers::mouse::{self as pm, MouseButton, Point};
use bricolo_providers::shell::Output;
use bricolo_providers::tts::TtsStatus;
use bricolo_providers::ui::{
    Button, Gauge, Panel, PanelKind, Switch, Tile, UiEvent as ProviderUiEvent, Widget, WidgetRow,
};
use bricolo_providers::window::{Rect, Screen, WindowInfo};

/// The generated bindings for one instance, and their pre-linked form.
pub type Bindings = Plugin;
pub type BindingsPre<T> = PluginPre<T>;

/// What one plugin's store holds.
pub struct HostState {
    pub caller: Caller,
    pub providers: Providers,
    /// Epoch ticks granted again after each awaited host call, so time
    /// spent waiting on a provider does not count against the plugin.
    pub budget_ticks: u64,
    wasi: WasiCtx,
    table: ResourceTable,
}

impl HostState {
    /// WASI sees only the plugin's data folder, mounted at `/data`. No
    /// sockets, no environment, no inherited stdio.
    pub fn new(caller: Caller, providers: Providers, data_dir: &Path, budget_ticks: u64) -> Self {
        let mut wasi = WasiCtx::builder();
        if let Err(e) = wasi.preopened_dir(data_dir, "/data", FsPerms::ReadWrite) {
            tracing::warn!(plugin = %caller.plugin, "cannot mount {}: {e:#}", data_dir.display());
        }
        Self {
            caller,
            providers,
            budget_ticks,
            wasi: wasi.build(),
            table: ResourceTable::new(),
        }
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl From<ProviderError> for CallError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::Unavailable => CallError::Unavailable,
            ProviderError::Timeout => CallError::Timeout,
            ProviderError::Denied => CallError::Denied,
            ProviderError::Failed(msg) => CallError::Failed(msg),
        }
    }
}

impl From<ProviderEvent> for Event {
    fn from(e: ProviderEvent) -> Self {
        match e {
            ProviderEvent::Timer(id) => Event::Timer(id),
            ProviderEvent::Hotkey(id) => Event::Hotkey(id),
            ProviderEvent::HotkeyReleased(id) => Event::HotkeyReleased(id),
            ProviderEvent::Ui(e) => Event::Ui(e.into()),
            ProviderEvent::App(e) => Event::App(types::AppEvent {
                change: match e.change {
                    AppChange::Launched => types::AppChange::Launched,
                    AppChange::Terminated => types::AppChange::Terminated,
                    AppChange::Activated => types::AppChange::Activated,
                },
                name: e.app.name,
                bundle_id: e.app.bundle_id,
                pid: e.app.pid,
            }),
            ProviderEvent::System(e) => Event::System(match e {
                SystemEvent::WillSleep => types::SystemEvent::WillSleep,
                SystemEvent::DidWake => types::SystemEvent::DidWake,
                SystemEvent::ScreenLocked => types::SystemEvent::ScreenLocked,
                SystemEvent::ScreenUnlocked => types::SystemEvent::ScreenUnlocked,
                SystemEvent::ScreensChanged => types::SystemEvent::ScreensChanged,
            }),
            ProviderEvent::File(e) => Event::File(types::FileEvent {
                watch: e.watch,
                path: e.path,
                change: match e.change {
                    FileChange::Created => types::FileChange::Created,
                    FileChange::Modified => types::FileChange::Modified,
                    FileChange::Removed => types::FileChange::Removed,
                },
            }),
        }
    }
}

impl From<ProviderUiEvent> for types::UiEvent {
    fn from(e: ProviderUiEvent) -> Self {
        match e {
            ProviderUiEvent::Switch { id, on } => Self::Switch(types::SwitchToggled { id, on }),
            ProviderUiEvent::Button { id } => Self::Button(id),
            ProviderUiEvent::Choice { panel, index } => {
                Self::Choice(types::ChoicePicked { panel, index })
            }
            ProviderUiEvent::Review {
                panel,
                accepted,
                text,
            } => Self::Review(types::ReviewDone {
                panel,
                accepted,
                text,
            }),
        }
    }
}

type Acc<U> = Accessor<U, HasSelf<HostState>>;

/// The providers and caller, copied out of the store before an await.
fn ctx<U>(acc: &Acc<U>) -> (Providers, Caller) {
    acc.with(|mut a| {
        let s = a.get();
        (s.providers.clone(), s.caller.clone())
    })
}

/// Restarts the plugin's CPU budget after an awaited host call.
fn resume<U>(acc: &Acc<U>) {
    acc.with(|mut a| {
        let ticks = a.get().budget_ticks;
        a.as_context_mut().set_epoch_deadline(ticks);
    });
}

impl types::Host for HostState {}

impl log::Host for HostState {
    fn write(&mut self, level: log::Level, message: String) {
        let level = match level {
            log::Level::Trace => LogLevel::Trace,
            log::Level::Debug => LogLevel::Debug,
            log::Level::Info => LogLevel::Info,
            log::Level::Warn => LogLevel::Warn,
            log::Level::Error => LogLevel::Error,
        };
        self.providers
            .log
            .write(&self.caller.plugin, level, &message);
    }
}

impl timer::Host for HostState {
    fn after(&mut self, ms: u32) -> u64 {
        self.providers.timer.after(&self.caller.plugin, ms)
    }

    fn every(&mut self, ms: u32) -> u64 {
        self.providers.timer.every(&self.caller.plugin, ms)
    }

    fn cancel(&mut self, id: u64) {
        self.providers.timer.cancel(&self.caller.plugin, id);
    }
}

impl<U> timer::HostWithStore<U> for HasSelf<HostState> {
    async fn sleep(acc: &Acc<U>, ms: u32) {
        let (p, _) = ctx(acc);
        p.timer.sleep(ms).await;
        resume(acc);
    }
}

impl kv::Host for HostState {
    fn get(&mut self, key: String) -> Option<String> {
        self.providers.kv.get(&self.caller.plugin, &key)
    }

    fn set(&mut self, key: String, value: String) -> Result<(), CallError> {
        Ok(self.providers.kv.set(&self.caller.plugin, &key, &value)?)
    }

    fn delete(&mut self, key: String) -> Result<(), CallError> {
        Ok(self.providers.kv.delete(&self.caller.plugin, &key)?)
    }

    fn keys(&mut self) -> Vec<String> {
        self.providers.kv.keys(&self.caller.plugin)
    }
}

impl hotkey::Host for HostState {}

impl<U> hotkey::HostWithStore<U> for HasSelf<HostState> {
    async fn bind(acc: &Acc<U>, id: String, chord: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.hotkey.bind(&c, id, chord).await;
        resume(acc);
        Ok(r?)
    }

    async fn unbind(acc: &Acc<U>, id: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.hotkey.unbind(&c, id).await;
        resume(acc);
        Ok(r?)
    }
}

impl tts::Host for HostState {}

impl<U> tts::HostWithStore<U> for HasSelf<HostState> {
    async fn speak(acc: &Acc<U>, text: String, voice: Option<String>) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.tts.speak(&c, text, voice).await;
        resume(acc);
        Ok(r?)
    }

    async fn stop(acc: &Acc<U>) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.tts.stop(&c).await;
        resume(acc);
        Ok(r?)
    }

    async fn status(acc: &Acc<U>) -> Result<tts::QueueStatus, CallError> {
        let (p, c) = ctx(acc);
        let r = p.tts.status(&c).await;
        resume(acc);
        let TtsStatus {
            speaking,
            queued,
            voice,
        } = r?;
        Ok(tts::QueueStatus {
            speaking,
            queued,
            voice,
        })
    }
}

impl speech::Host for HostState {}

impl<U> speech::HostWithStore<U> for HasSelf<HostState> {
    async fn transcribe(acc: &Acc<U>, audio: Vec<u8>, format: String) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.speech.transcribe(&c, audio, format).await;
        resume(acc);
        Ok(r?)
    }

    async fn transcribe_file(acc: &Acc<U>, path: String) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.speech.transcribe_file(&c, path).await;
        resume(acc);
        Ok(r?)
    }
}

impl disk::Host for HostState {}

impl<U> disk::HostWithStore<U> for HasSelf<HostState> {
    async fn read(acc: &Acc<U>, path: String) -> Result<Vec<u8>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.disk.read(&c, path).await;
        resume(acc);
        Ok(r?)
    }

    async fn write(acc: &Acc<U>, path: String, data: Vec<u8>) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.disk.write(&c, path, data).await;
        resume(acc);
        Ok(r?)
    }

    async fn list(acc: &Acc<U>, path: String) -> Result<Vec<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.disk.list(&c, path).await;
        resume(acc);
        Ok(r?)
    }
}

impl clipboard::Host for HostState {}

impl<U> clipboard::HostWithStore<U> for HasSelf<HostState> {
    async fn get(acc: &Acc<U>) -> Result<Option<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.clipboard.get(&c).await;
        resume(acc);
        Ok(r?)
    }

    async fn set(acc: &Acc<U>, text: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.clipboard.set(&c, text).await;
        resume(acc);
        Ok(r?)
    }

    async fn formats(acc: &Acc<U>) -> Result<Vec<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.clipboard.formats(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn get_image(acc: &Acc<U>) -> Result<Option<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.clipboard.get_image(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_image(acc: &Acc<U>, path: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.clipboard.set_image(&c, path).await;
        resume(acc);
        Ok(r?)
    }
    async fn get_files(acc: &Acc<U>) -> Result<Vec<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.clipboard.get_files(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_files(acc: &Acc<U>, paths: Vec<String>) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.clipboard.set_files(&c, paths).await;
        resume(acc);
        Ok(r?)
    }
}

impl From<Rect> for window::Rect {
    fn from(r: Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
    }
}

impl From<window::Rect> for Rect {
    fn from(r: window::Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        }
    }
}

impl From<WindowInfo> for window::WindowInfo {
    fn from(w: WindowInfo) -> Self {
        Self {
            id: w.id,
            app: w.app,
            title: w.title,
            frame: w.frame.into(),
        }
    }
}

impl From<Screen> for window::Screen {
    fn from(s: Screen) -> Self {
        Self {
            id: s.id,
            frame: s.frame.into(),
            visible_frame: s.visible_frame.into(),
            main: s.main,
        }
    }
}

impl window::Host for HostState {}

impl<U> window::HostWithStore<U> for HasSelf<HostState> {
    async fn list(acc: &Acc<U>) -> Result<Vec<window::WindowInfo>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.window.list(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }

    async fn focused(acc: &Acc<U>) -> Result<Option<window::WindowInfo>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.window.focused(&c).await;
        resume(acc);
        Ok(r?.map(Into::into))
    }

    async fn focus(acc: &Acc<U>, id: u64) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.window.focus(&c, id).await;
        resume(acc);
        Ok(r?)
    }

    async fn set_frame(acc: &Acc<U>, id: u64, frame: window::Rect) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.window.set_frame(&c, id, frame.into()).await;
        resume(acc);
        Ok(r?)
    }

    async fn close(acc: &Acc<U>, id: u64) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.window.close(&c, id).await;
        resume(acc);
        Ok(r?)
    }

    async fn set_minimized(acc: &Acc<U>, id: u64, minimized: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.window.set_minimized(&c, id, minimized).await;
        resume(acc);
        Ok(r?)
    }

    async fn screens(acc: &Acc<U>) -> Result<Vec<window::Screen>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.window.screens(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
}

impl From<AppInfo> for app::AppInfo {
    fn from(a: AppInfo) -> Self {
        Self {
            name: a.name,
            bundle_id: a.bundle_id,
            pid: a.pid,
        }
    }
}

impl app::Host for HostState {}

impl<U> app::HostWithStore<U> for HasSelf<HostState> {
    async fn launch(acc: &Acc<U>, name: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.app.launch(&c, name).await;
        resume(acc);
        Ok(r?)
    }

    async fn quit(acc: &Acc<U>, name: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.app.quit(&c, name).await;
        resume(acc);
        Ok(r?)
    }

    async fn set_hidden(acc: &Acc<U>, name: String, hidden: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.app.set_hidden(&c, name, hidden).await;
        resume(acc);
        Ok(r?)
    }

    async fn open_url(acc: &Acc<U>, url: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.app.open_url(&c, url).await;
        resume(acc);
        Ok(r?)
    }

    async fn running(acc: &Acc<U>) -> Result<Vec<app::AppInfo>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.app.running(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }

    async fn frontmost(acc: &Acc<U>) -> Result<Option<app::AppInfo>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.app.frontmost(&c).await;
        resume(acc);
        Ok(r?.map(Into::into))
    }

    async fn watch(acc: &Acc<U>, on: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.app.watch(&c, on).await;
        resume(acc);
        Ok(r?)
    }
}

impl notify::Host for HostState {}

impl<U> notify::HostWithStore<U> for HasSelf<HostState> {
    async fn show(acc: &Acc<U>, title: String, body: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.notify.show(&c, title, body).await;
        resume(acc);
        Ok(r?)
    }
}

impl shell::Host for HostState {}

impl<U> shell::HostWithStore<U> for HasSelf<HostState> {
    async fn run(
        acc: &Acc<U>,
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<shell::Output, CallError> {
        let (p, c) = ctx(acc);
        let r = p.shell.run(&c, program, args, env).await;
        resume(acc);
        let Output {
            code,
            stdout,
            stderr,
        } = r?;
        Ok(shell::Output {
            code,
            stdout,
            stderr,
        })
    }
}

impl http::Host for HostState {}

impl<U> http::HostWithStore<U> for HasSelf<HostState> {
    async fn send(acc: &Acc<U>, request: http::Request) -> Result<http::Response, CallError> {
        let (p, c) = ctx(acc);
        let request = HttpRequest {
            method: request.method,
            url: request.url,
            headers: request.headers,
            body: request.body,
            timeout: request
                .timeout_ms
                .map(|ms| std::time::Duration::from_millis(ms.into())),
        };
        let r = p.http.send(&c, request).await;
        resume(acc);
        let HttpResponse {
            status,
            headers,
            body,
        } = r?;
        Ok(http::Response {
            status,
            headers,
            body,
        })
    }
}

impl From<ui::Tile> for Tile {
    fn from(t: ui::Tile) -> Self {
        Self {
            title: t.title,
            status_text: t.status_text,
            switches: t
                .switches
                .into_iter()
                .map(|s| Switch {
                    id: s.id,
                    label: s.label,
                    on: s.on,
                })
                .collect(),
            buttons: t
                .buttons
                .into_iter()
                .map(|b| Button {
                    id: b.id,
                    label: b.label,
                })
                .collect(),
        }
    }
}

fn widget_rows(rows: Vec<ui::WidgetRow>) -> Vec<WidgetRow> {
    rows.into_iter()
        .map(|r| WidgetRow {
            label: r.label,
            value: r.value,
        })
        .collect()
}

impl From<ui::Widget> for Widget {
    fn from(w: ui::Widget) -> Self {
        Self {
            title: w.title,
            gauges: Vec::new(),
            rows: widget_rows(w.rows),
        }
    }
}

impl From<ui::GaugeWidget> for Widget {
    fn from(w: ui::GaugeWidget) -> Self {
        Self {
            title: w.title,
            gauges: w
                .gauges
                .into_iter()
                .map(|g| Gauge {
                    label: g.label,
                    value: g.value,
                    fill: g.fill,
                    marks: g.marks,
                    now: g.now,
                    note: g.note,
                })
                .collect(),
            rows: widget_rows(w.rows),
        }
    }
}

impl From<ui::Panel> for Panel {
    fn from(p: ui::Panel) -> Self {
        Self {
            kind: match p.kind {
                ui::PanelKind::Caption => PanelKind::Caption,
                ui::PanelKind::ChoiceList => PanelKind::ChoiceList,
                ui::PanelKind::Toast => PanelKind::Toast,
                ui::PanelKind::Review => PanelKind::Review,
            },
            title: p.title,
            text: p.text,
            choices: p.choices,
            timeout_ms: p.timeout_ms,
        }
    }
}

impl ui::Host for HostState {
    fn set_tile(&mut self, tile: ui::Tile) -> Result<(), CallError> {
        Ok(self.providers.ui.set_tile(&self.caller, tile.into())?)
    }

    fn clear_tile(&mut self) -> Result<(), CallError> {
        Ok(self.providers.ui.clear_tile(&self.caller)?)
    }

    fn set_widget(&mut self, widget: ui::Widget) -> Result<(), CallError> {
        Ok(self.providers.ui.set_widget(&self.caller, widget.into())?)
    }

    fn set_gauge_widget(&mut self, widget: ui::GaugeWidget) -> Result<(), CallError> {
        Ok(self.providers.ui.set_widget(&self.caller, widget.into())?)
    }

    fn clear_widget(&mut self) -> Result<(), CallError> {
        Ok(self.providers.ui.clear_widget(&self.caller)?)
    }

    fn show_panel(&mut self, panel: ui::Panel) -> Result<String, CallError> {
        Ok(self.providers.ui.show_panel(&self.caller, panel.into())?)
    }

    fn hide_panel(&mut self, id: String) -> Result<(), CallError> {
        Ok(self.providers.ui.hide_panel(&self.caller, &id)?)
    }
}

impl keyboard::Host for HostState {}

impl<U> keyboard::HostWithStore<U> for HasSelf<HostState> {
    async fn keystroke(acc: &Acc<U>, mods: Vec<String>, key: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.keyboard.keystroke(&c, mods, key).await;
        resume(acc);
        Ok(r?)
    }

    async fn type_text(acc: &Acc<U>, text: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.keyboard.type_text(&c, text).await;
        resume(acc);
        Ok(r?)
    }
}

impl From<Point> for mouse::Point {
    fn from(p: Point) -> Self {
        Self { x: p.x, y: p.y }
    }
}

impl From<mouse::Point> for Point {
    fn from(p: mouse::Point) -> Self {
        Self { x: p.x, y: p.y }
    }
}

impl From<mouse::Button> for MouseButton {
    fn from(b: mouse::Button) -> Self {
        match b {
            mouse::Button::Left => Self::Left,
            mouse::Button::Right => Self::Right,
            mouse::Button::Middle => Self::Middle,
        }
    }
}

impl mouse::Host for HostState {}

impl<U> mouse::HostWithStore<U> for HasSelf<HostState> {
    async fn position(acc: &Acc<U>) -> Result<mouse::Point, CallError> {
        let (p, c) = ctx(acc);
        let r = p.mouse.position(&c).await;
        resume(acc);
        Ok(r?.into())
    }

    async fn move_to(acc: &Acc<U>, to: mouse::Point) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.mouse.move_to(&c, to.into()).await;
        resume(acc);
        Ok(r?)
    }

    async fn click(acc: &Acc<U>, button: mouse::Button, count: u8) -> Result<(), CallError> {
        pm::check_click_count(count)?;
        let (p, c) = ctx(acc);
        let r = p.mouse.click(&c, button.into(), count).await;
        resume(acc);
        Ok(r?)
    }

    async fn scroll(acc: &Acc<U>, dx: i32, dy: i32) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.mouse.scroll(&c, dx, dy).await;
        resume(acc);
        Ok(r?)
    }
}

impl screen::Host for HostState {}

impl<U> screen::HostWithStore<U> for HasSelf<HostState> {
    async fn capture(acc: &Acc<U>, area: Option<window::Rect>) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let area = area.map(Rect::from);
        let r = match area.as_ref().map(bricolo_providers::screen::check_area) {
            Some(Err(e)) => Err(e),
            _ => p.screen.capture(&c, area).await,
        };
        resume(acc);
        Ok(r?)
    }
}

impl selection::Host for HostState {}

impl<U> selection::HostWithStore<U> for HasSelf<HostState> {
    async fn get_text(acc: &Acc<U>) -> Result<Option<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.selection.get_text(&c).await;
        resume(acc);
        Ok(r?)
    }
}

impl secrets::Host for HostState {}

impl<U> secrets::HostWithStore<U> for HasSelf<HostState> {
    async fn get(acc: &Acc<U>, name: String) -> Result<Option<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.secrets.get(&c, name).await;
        resume(acc);
        Ok(r?)
    }

    async fn set(acc: &Acc<U>, name: String, value: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.secrets.set(&c, name, value).await;
        resume(acc);
        Ok(r?)
    }

    async fn delete(acc: &Acc<U>, name: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.secrets.delete(&c, name).await;
        resume(acc);
        Ok(r?)
    }
}

impl From<mic::RecordOptions> for RecordOptions {
    fn from(o: mic::RecordOptions) -> Self {
        Self {
            max_secs: o.max_secs,
            format: match o.format {
                mic::AudioFormat::Wav16kMono => AudioFormat::Wav16kMono,
            },
        }
    }
}

impl mic::Host for HostState {}

impl<U> mic::HostWithStore<U> for HasSelf<HostState> {
    async fn record_start(acc: &Acc<U>, options: mic::RecordOptions) -> Result<u64, CallError> {
        let (p, c) = ctx(acc);
        let r = p.mic.record_start(&c, options.into()).await;
        resume(acc);
        Ok(r?)
    }

    async fn record_stop(acc: &Acc<U>, id: u64) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.mic.record_stop(&c, id).await;
        resume(acc);
        Ok(r?)
    }

    async fn record_cancel(acc: &Acc<U>, id: u64) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.mic.record_cancel(&c, id).await;
        resume(acc);
        Ok(r?)
    }
}

impl From<audio::PlayOptions> for PlayOptions {
    fn from(o: audio::PlayOptions) -> Self {
        Self {
            volume: o.volume,
            looping: o.looping,
        }
    }
}

impl From<Sound> for audio::Sound {
    fn from(s: Sound) -> Self {
        Self {
            id: s.id,
            path: s.path,
            volume: s.volume,
            looping: s.looping,
        }
    }
}

impl camera::Host for HostState {}

impl<U> camera::HostWithStore<U> for HasSelf<HostState> {
    async fn snapshot(acc: &Acc<U>) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.camera.snapshot(&c).await;
        resume(acc);
        Ok(r?)
    }
}

impl audio::Host for HostState {}

impl<U> audio::HostWithStore<U> for HasSelf<HostState> {
    async fn play(
        acc: &Acc<U>,
        path: String,
        options: audio::PlayOptions,
    ) -> Result<u64, CallError> {
        let (p, c) = ctx(acc);
        let r = p.audio.play(&c, path, options.into()).await;
        resume(acc);
        Ok(r?)
    }

    async fn set_volume(acc: &Acc<U>, id: u64, volume: u8) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.audio.set_volume(&c, id, volume).await;
        resume(acc);
        Ok(r?)
    }

    async fn stop(acc: &Acc<U>, id: u64) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.audio.stop(&c, id).await;
        resume(acc);
        Ok(r?)
    }

    async fn sounds(acc: &Acc<U>) -> Result<Vec<audio::Sound>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.audio.sounds(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
}

impl system::Host for HostState {}

impl<U> system::HostWithStore<U> for HasSelf<HostState> {
    async fn idle_seconds(acc: &Acc<U>) -> Result<u64, CallError> {
        let (p, c) = ctx(acc);
        let r = p.system.idle_seconds(&c).await;
        resume(acc);
        Ok(r?)
    }

    async fn power(acc: &Acc<U>) -> Result<system::PowerInfo, CallError> {
        let (p, c) = ctx(acc);
        let r = p.system.power(&c).await;
        resume(acc);
        let power = r?;
        Ok(system::PowerInfo {
            battery: power.battery,
            plugged_in: power.plugged_in,
        })
    }

    async fn keep_awake(acc: &Acc<U>, on: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.system.keep_awake(&c, on).await;
        resume(acc);
        Ok(r?)
    }

    async fn watch(acc: &Acc<U>, on: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.system.watch(&c, on).await;
        resume(acc);
        Ok(r?)
    }
}

impl files::Host for HostState {}

impl<U> files::HostWithStore<U> for HasSelf<HostState> {
    async fn watch(acc: &Acc<U>, path: String) -> Result<u64, CallError> {
        let (p, c) = ctx(acc);
        let r = p.files.watch(&c, path).await;
        resume(acc);
        Ok(r?)
    }

    async fn unwatch(acc: &Acc<U>, id: u64) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.files.unwatch(&c, id).await;
        resume(acc);
        Ok(r?)
    }
}

impl From<CcSession> for claude_code::Session {
    fn from(s: CcSession) -> Self {
        Self {
            id: s.id,
            dir: s.dir,
            state: match s.state {
                CcState::Idle => claude_code::SessionState::Idle,
                CcState::Working => claude_code::SessionState::Working,
                CcState::Waiting => claude_code::SessionState::Waiting,
                CcState::Finished => claude_code::SessionState::Finished,
                CcState::Failed => claude_code::SessionState::Failed,
            },
            last_result: s.last_result,
            title: s.title,
            started: s.started,
            updated: s.updated,
        }
    }
}

impl claude_code::Host for HostState {}

impl<U> claude_code::HostWithStore<U> for HasSelf<HostState> {
    async fn start(
        acc: &Acc<U>,
        dir: String,
        prompt: String,
    ) -> Result<claude_code::Session, CallError> {
        let (p, c) = ctx(acc);
        let r = p.claude_code.start(&c, dir, prompt).await;
        resume(acc);
        Ok(r?.into())
    }

    async fn send(
        acc: &Acc<U>,
        id: String,
        prompt: String,
    ) -> Result<claude_code::Session, CallError> {
        let (p, c) = ctx(acc);
        let r = p.claude_code.send(&c, id, prompt).await;
        resume(acc);
        Ok(r?.into())
    }

    async fn list(acc: &Acc<U>) -> Result<Vec<claude_code::Session>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.claude_code.list(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }

    async fn status(acc: &Acc<U>, id: String) -> Result<claude_code::Session, CallError> {
        let (p, c) = ctx(acc);
        let r = p.claude_code.status(&c, id).await;
        resume(acc);
        Ok(r?.into())
    }

    async fn stop(acc: &Acc<U>, id: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.claude_code.stop(&c, id).await;
        resume(acc);
        Ok(r?)
    }
}

impl From<pax::AxApp> for accessibility::AxApp {
    fn from(a: pax::AxApp) -> Self {
        Self {
            name: a.name,
            bundle_id: a.bundle_id,
            pid: a.pid,
            frontmost: a.frontmost,
        }
    }
}

impl From<pax::AxWindow> for accessibility::AxWindow {
    fn from(w: pax::AxWindow) -> Self {
        Self {
            index: w.index,
            title: w.title,
            frame: w.frame.map(Into::into),
            focused: w.focused,
            minimized: w.minimized,
        }
    }
}

impl From<pax::AppState> for accessibility::AppState {
    fn from(s: pax::AppState) -> Self {
        Self {
            app: s.app,
            pid: s.pid,
            window: s.window,
            tree: s.tree,
            element_count: s.element_count,
            truncated: s.truncated,
            focused: s.focused,
            screenshot: s.screenshot,
        }
    }
}

impl From<pax::Element> for accessibility::Element {
    fn from(e: pax::Element) -> Self {
        Self {
            index: e.index,
            role: e.role,
            name: e.name,
            value: e.value,
            frame: e.frame.map(Into::into),
            enabled: e.enabled,
            focused: e.focused,
            actions: e.actions,
        }
    }
}

impl From<pax::ActionResult> for accessibility::ActionResult {
    fn from(r: pax::ActionResult) -> Self {
        Self {
            verification: match r.verification {
                pax::Verification::Verified => accessibility::Verification::Verified,
                pax::Verification::Unverified => accessibility::Verification::Unverified,
                pax::Verification::SuspectedNoop => accessibility::Verification::SuspectedNoop,
                pax::Verification::Mismatch => accessibility::Verification::Mismatch,
            },
            detail: r.detail,
            state: r.state.into(),
        }
    }
}

impl From<bricolo_providers::process::ProcessInfo> for process::ProcessInfo {
    fn from(p: bricolo_providers::process::ProcessInfo) -> Self {
        Self {
            pid: p.pid,
            parent: p.parent,
            name: p.name,
            command: p.command,
            user: p.user,
            cpu: p.cpu,
            memory: p.memory,
            started: p.started,
        }
    }
}

impl From<bricolo_providers::virtual_screen::VirtualScreenInfo>
    for virtual_screen::VirtualScreenInfo
{
    fn from(v: bricolo_providers::virtual_screen::VirtualScreenInfo) -> Self {
        Self {
            id: v.id,
            name: v.name,
            width: v.width,
            height: v.height,
            pids: v.pids,
        }
    }
}

impl accessibility::Host for HostState {}

impl<U> accessibility::HostWithStore<U> for HasSelf<HostState> {
    async fn list_apps(acc: &Acc<U>) -> Result<Vec<accessibility::AxApp>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.list_apps(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
    async fn list_windows(
        acc: &Acc<U>,
        app: String,
    ) -> Result<Vec<accessibility::AxWindow>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.list_windows(&c, app).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
    async fn snapshot(
        acc: &Acc<U>,
        app: String,
        window: Option<u32>,
        query: Option<String>,
        element: Option<u32>,
        screenshot: Option<bool>,
    ) -> Result<accessibility::AppState, CallError> {
        let (p, c) = ctx(acc);
        let r = p
            .accessibility
            .snapshot(&c, app, window, query, element, screenshot)
            .await;
        resume(acc);
        Ok(r?.into())
    }
    async fn find(
        acc: &Acc<U>,
        app: String,
        text: String,
    ) -> Result<Vec<accessibility::Element>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.find(&c, app, text).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
    async fn click(
        acc: &Acc<U>,
        app: String,
        element: u32,
    ) -> Result<accessibility::ActionResult, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.click(&c, app, element).await;
        resume(acc);
        Ok(r?.into())
    }
    async fn set_value(
        acc: &Acc<U>,
        app: String,
        element: u32,
        value: String,
    ) -> Result<accessibility::ActionResult, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.set_value(&c, app, element, value).await;
        resume(acc);
        Ok(r?.into())
    }
    async fn type_text(
        acc: &Acc<U>,
        app: String,
        element: Option<u32>,
        text: String,
    ) -> Result<accessibility::ActionResult, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.type_text(&c, app, element, text).await;
        resume(acc);
        Ok(r?.into())
    }
    async fn press_key(
        acc: &Acc<U>,
        app: String,
        key: String,
    ) -> Result<accessibility::ActionResult, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.press_key(&c, app, key).await;
        resume(acc);
        Ok(r?.into())
    }
    async fn perform(
        acc: &Acc<U>,
        app: String,
        element: u32,
        action: String,
    ) -> Result<accessibility::ActionResult, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.perform(&c, app, element, action).await;
        resume(acc);
        Ok(r?.into())
    }
    async fn menu(
        acc: &Acc<U>,
        app: String,
        path: Vec<String>,
    ) -> Result<accessibility::ActionResult, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.menu(&c, app, path).await;
        resume(acc);
        Ok(r?.into())
    }
    async fn wait(
        acc: &Acc<U>,
        app: String,
        text: String,
        timeout_ms: Option<u32>,
    ) -> Result<Vec<accessibility::Element>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.accessibility.wait(&c, app, text, timeout_ms).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
}

impl process::Host for HostState {}

impl<U> process::HostWithStore<U> for HasSelf<HostState> {
    async fn list(acc: &Acc<U>) -> Result<Vec<process::ProcessInfo>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.process.list(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
    async fn get(acc: &Acc<U>, pid: u32) -> Result<Option<process::ProcessInfo>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.process.get(&c, pid).await;
        resume(acc);
        Ok(r?.map(Into::into))
    }
    async fn stop(acc: &Acc<U>, pid: u32) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.process.stop(&c, pid).await;
        resume(acc);
        Ok(r?)
    }
    async fn kill(acc: &Acc<U>, pid: u32) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.process.kill(&c, pid).await;
        resume(acc);
        Ok(r?)
    }
}

impl device::Host for HostState {}

impl<U> device::HostWithStore<U> for HasSelf<HostState> {
    async fn volume(acc: &Acc<U>) -> Result<u8, CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.volume(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn muted(acc: &Acc<U>) -> Result<bool, CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.muted(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn brightness(acc: &Acc<U>) -> Result<u8, CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.brightness(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn wifi(acc: &Acc<U>) -> Result<bool, CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.wifi(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn dark_mode(acc: &Acc<U>) -> Result<bool, CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.dark_mode(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn focus_mode(acc: &Acc<U>) -> Result<bool, CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.focus_mode(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_volume(acc: &Acc<U>, level: u8) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.set_volume(&c, level).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_muted(acc: &Acc<U>, muted: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.set_muted(&c, muted).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_brightness(acc: &Acc<U>, level: u8) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.set_brightness(&c, level).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_wifi(acc: &Acc<U>, on: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.set_wifi(&c, on).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_dark_mode(acc: &Acc<U>, on: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.set_dark_mode(&c, on).await;
        resume(acc);
        Ok(r?)
    }
    async fn set_focus_mode(acc: &Acc<U>, on: bool) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.set_focus_mode(&c, on).await;
        resume(acc);
        Ok(r?)
    }
    async fn lock(acc: &Acc<U>) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.lock(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn sleep(acc: &Acc<U>) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.device.sleep(&c).await;
        resume(acc);
        Ok(r?)
    }
}

impl scripting::Host for HostState {}

impl<U> scripting::HostWithStore<U> for HasSelf<HostState> {
    async fn run_applescript(acc: &Acc<U>, source: String) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.scripting.run_applescript(&c, source).await;
        resume(acc);
        Ok(r?)
    }
    async fn run_jxa(acc: &Acc<U>, source: String) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.scripting.run_jxa(&c, source).await;
        resume(acc);
        Ok(r?)
    }
    async fn list_shortcuts(acc: &Acc<U>) -> Result<Vec<String>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.scripting.list_shortcuts(&c).await;
        resume(acc);
        Ok(r?)
    }
    async fn run_shortcut(
        acc: &Acc<U>,
        name: String,
        input: Option<String>,
    ) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.scripting.run_shortcut(&c, name, input).await;
        resume(acc);
        Ok(r?)
    }
    async fn run_powershell(acc: &Acc<U>, source: String) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.scripting.run_powershell(&c, source).await;
        resume(acc);
        Ok(r?)
    }
}

impl virtual_screen::Host for HostState {}

impl<U> virtual_screen::HostWithStore<U> for HasSelf<HostState> {
    async fn create(
        acc: &Acc<U>,
        width: u32,
        height: u32,
    ) -> Result<virtual_screen::VirtualScreenInfo, CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.create(&c, width, height).await;
        resume(acc);
        Ok(r?.into())
    }
    async fn list(acc: &Acc<U>) -> Result<Vec<virtual_screen::VirtualScreenInfo>, CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.list(&c).await;
        resume(acc);
        Ok(r?.into_iter().map(Into::into).collect())
    }
    async fn launch(
        acc: &Acc<U>,
        id: u32,
        program: String,
        args: Vec<String>,
    ) -> Result<u32, CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.launch(&c, id, program, args).await;
        resume(acc);
        Ok(r?)
    }
    async fn capture(acc: &Acc<U>, id: u32) -> Result<String, CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.capture(&c, id).await;
        resume(acc);
        Ok(r?)
    }
    async fn click(acc: &Acc<U>, id: u32, x: i32, y: i32) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.click(&c, id, x, y).await;
        resume(acc);
        Ok(r?)
    }
    async fn type_text(acc: &Acc<U>, id: u32, text: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.type_text(&c, id, text).await;
        resume(acc);
        Ok(r?)
    }
    async fn press_key(acc: &Acc<U>, id: u32, key: String) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.press_key(&c, id, key).await;
        resume(acc);
        Ok(r?)
    }
    async fn destroy(acc: &Acc<U>, id: u32) -> Result<(), CallError> {
        let (p, c) = ctx(acc);
        let r = p.virtual_screen.destroy(&c, id).await;
        resume(acc);
        Ok(r?)
    }
}
