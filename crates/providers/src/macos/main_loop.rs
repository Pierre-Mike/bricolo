//! The macOS main thread: the AppKit event loop (through tao), global
//! hotkeys, the Fn key tap, and the screen list. Other threads reach it
//! through [`MainHandle`]; replies come back on Tokio oneshot channels.

use std::sync::{Arc, Mutex};

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tao::platform::run_return::EventLoopExtRunReturn;
use tokio::sync::oneshot;

use super::{flip_rect, fn_key};
use crate::window::Screen;

pub enum MainMsg {
    Register {
        hotkey: HotKey,
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    Unregister {
        hotkey: HotKey,
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    /// Starts or stops watching the Fn key.
    WatchFn {
        on: bool,
        reply: Option<oneshot::Sender<Result<(), String>>>,
    },
    Screens {
        reply: oneshot::Sender<Vec<Screen>>,
    },
    Quit,
}

/// Called on the main thread with the OS id of a hotkey, and `true` when it
/// was pressed or `false` when it was released. The Fn key comes as
/// [`fn_key::OS_ID`].
pub type PressHandler = Arc<dyn Fn(u32, bool) + Send + Sync>;

#[derive(Clone)]
pub struct MainHandle {
    proxy: EventLoopProxy<MainMsg>,
    on_press: Arc<Mutex<Option<PressHandler>>>,
}

const GONE: &str = "the main-thread loop has stopped";

impl MainHandle {
    async fn ask<T>(&self, msg: MainMsg, rx: oneshot::Receiver<T>) -> Result<T, String> {
        self.proxy.send_event(msg).map_err(|_| GONE.to_string())?;
        rx.await.map_err(|_| GONE.to_string())
    }

    pub async fn register(&self, hotkey: HotKey) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        let msg = MainMsg::Register {
            hotkey,
            reply: Some(reply),
        };
        self.ask(msg, rx).await?
    }

    pub async fn unregister(&self, hotkey: HotKey) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        let msg = MainMsg::Unregister {
            hotkey,
            reply: Some(reply),
        };
        self.ask(msg, rx).await?
    }

    /// Unregisters without waiting, for callers that cannot await.
    pub fn unregister_later(&self, hotkey: HotKey) {
        let _ = self.proxy.send_event(MainMsg::Unregister {
            hotkey,
            reply: None,
        });
    }

    /// Watches the Fn key, or stops. Fails when macOS refuses the event
    /// tap, which needs the Input Monitoring or Accessibility permission.
    pub async fn watch_fn(&self, on: bool) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        let msg = MainMsg::WatchFn {
            on,
            reply: Some(reply),
        };
        self.ask(msg, rx).await?
    }

    /// Stops watching the Fn key without waiting.
    pub fn unwatch_fn_later(&self) {
        let _ = self.proxy.send_event(MainMsg::WatchFn {
            on: false,
            reply: None,
        });
    }

    pub async fn screens(&self) -> Result<Vec<Screen>, String> {
        let (reply, rx) = oneshot::channel();
        self.ask(MainMsg::Screens { reply }, rx).await
    }

    /// Replaces the hotkey press handler.
    pub fn on_press(&self, handler: PressHandler) {
        *self.on_press.lock().unwrap() = Some(handler);
    }
}

/// Stops the event loop when the node thread ends, even by a panic.
struct QuitOnDrop(EventLoopProxy<MainMsg>);

impl Drop for QuitOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send_event(MainMsg::Quit);
    }
}

/// Runs the event loop on this thread, which must be the main thread, and
/// `node` on a new thread. Returns `node`'s result once it ends.
pub fn run<T: Send + 'static>(node: impl FnOnce(MainHandle) -> T + Send + 'static) -> T {
    let mut event_loop = EventLoopBuilder::<MainMsg>::with_user_event().build();
    // No Dock icon and no menu bar: the node is a background agent.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);
    // tao activates the app at launch by default, which takes focus from
    // the app in front, so the tiler and `app.frontmost` would see `bricolo`.
    event_loop.set_activate_ignoring_other_apps(false);
    let handle = MainHandle {
        proxy: event_loop.create_proxy(),
        on_press: Arc::default(),
    };
    let presses = handle.on_press.clone();
    let quit = QuitOnDrop(event_loop.create_proxy());
    let worker = std::thread::Builder::new()
        .name("bricolo-node".into())
        .spawn(move || {
            let _quit = quit;
            node(handle)
        })
        .expect("cannot start the node thread");

    let manager = GlobalHotKeyManager::new()
        .map_err(|e| tracing::error!("hotkeys are off: {e}"))
        .ok();
    let carbon = presses.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        let handler = carbon.lock().unwrap().clone();
        if let Some(handler) = handler {
            handler(event.id, event.state == HotKeyState::Pressed);
        }
    }));
    let mut fn_tap: Option<fn_key::Tap> = None;
    event_loop.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        let Event::UserEvent(msg) = event else {
            return;
        };
        match msg {
            MainMsg::Register { hotkey, reply } => {
                let result = match &manager {
                    Some(m) => m.register(hotkey).map_err(|e| e.to_string()),
                    None => Err("the hotkey manager did not start".into()),
                };
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            MainMsg::Unregister { hotkey, reply } => {
                let result = match &manager {
                    Some(m) => m.unregister(hotkey).map_err(|e| e.to_string()),
                    None => Ok(()),
                };
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            MainMsg::WatchFn { on, reply } => {
                let result = match (on, fn_tap.is_some()) {
                    (true, false) => fn_key::Tap::start(presses.clone()).map(|t| fn_tap = Some(t)),
                    (false, true) => {
                        fn_tap = None;
                        Ok(())
                    }
                    _ => Ok(()),
                };
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
            MainMsg::Screens { reply } => {
                let _ = reply.send(screens());
            }
            MainMsg::Quit => *flow = ControlFlow::Exit,
        }
    });
    GlobalHotKeyEvent::set_event_handler(None::<fn(GlobalHotKeyEvent)>);
    drop(fn_tap);
    drop(manager);
    match worker.join() {
        Ok(out) => out,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn screens() -> Vec<Screen> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let screens = NSScreen::screens(mtm);
    // The first screen holds the menu bar; AppKit coordinates start there.
    let Some(main) = screens.firstObject() else {
        return Vec::new();
    };
    let main_height = main.frame().size.height;
    screens
        .iter()
        .enumerate()
        .map(|(i, screen)| {
            let f = screen.frame();
            let v = screen.visibleFrame();
            Screen {
                id: i as u32,
                frame: flip_rect(
                    f.origin.x,
                    f.origin.y,
                    f.size.width,
                    f.size.height,
                    main_height,
                ),
                visible_frame: flip_rect(
                    v.origin.x,
                    v.origin.y,
                    v.size.width,
                    v.size.height,
                    main_height,
                ),
                main: i == 0,
            }
        })
        .collect()
}
