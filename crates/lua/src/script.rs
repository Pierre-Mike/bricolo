//! One running script: its Lua state, and the task that turns provider
//! events into callback coroutines, restarts the script once after an
//! uncaught error, and disposes it on stop.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bricolo_providers::ui::UiEvent;
use bricolo_providers::{Caller, ProviderEvent, Providers};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Nil, Table, Value};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};

use crate::clock::{Clock, budgeted};
use crate::sandbox::{self, CALLBACKS};
use crate::stdlib::{self, UI_BUTTON, UI_SWITCH, ui_choices_key, ui_panel_key};
use crate::tier::Limits;

/// How long `dispose` may run on stop or reload.
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    /// Failed twice, or failed while starting. Stays off until its file
    /// changes or it is reloaded.
    Off,
    /// Stopped on purpose: shutdown.
    Stopped,
    /// Turned off in the hub. Stays off, across restarts, until turned on.
    Disabled,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScriptInfo {
    /// The file name without `.lua`.
    pub name: String,
    pub state: State,
    /// Why it is off, or why the last reload was refused or failed.
    pub error: Option<String>,
    /// Restarts since the last load: 0 or 1.
    pub restarts: u32,
}

pub type SharedInfo = Arc<Mutex<ScriptInfo>>;

/// Files a script loaded with `require`, `dofile` or `loadfile`, with the
/// source it read. A change to one of them reloads the script.
pub type Modules = Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>;

/// What every script shares.
pub struct Env {
    pub providers: Providers,
    pub limits: Limits,
    /// The canonical `scripts/` folder.
    pub dir: PathBuf,
}

/// Everything needed to start, or restart, one script.
pub struct Spec {
    pub name: String,
    /// `<name>.lua`: the script's identity for providers, kv and logs.
    pub caller: Caller,
    pub source: Vec<u8>,
}

/// What the `bricolo` functions of one Lua state reach.
pub struct Ctx {
    pub name: String,
    pub caller: Caller,
    pub providers: Providers,
    pub dir: PathBuf,
    pub modules: Modules,
    pub clock: Arc<Clock>,
    pub next_hotkey: AtomicU64,
}

pub enum Cmd {
    Event(ProviderEvent),
    Stop(oneshot::Sender<()>),
}

type Call = BoxFuture<'static, mlua::Result<()>>;

/// A started Lua state.
pub struct Live {
    lua: Lua,
    ctx: Arc<Ctx>,
}

/// Uncaught errors as one readable string.
fn describe(e: &mlua::Error) -> String {
    e.to_string()
}

fn global_fn(lua: &Lua, name: &str) -> Option<Function> {
    lua.globals().get::<Option<Function>>(name).ok().flatten()
}

impl Live {
    /// Builds the sandbox, runs the script body, then its `init()` if it
    /// defines one. Both run as coroutines under the init budget.
    pub async fn start(env: &Env, spec: &Spec, modules: &Modules) -> Result<Self, String> {
        modules.lock().unwrap().clear();
        let ctx = Arc::new(Ctx {
            name: spec.name.clone(),
            caller: spec.caller.clone(),
            providers: env.providers.clone(),
            dir: env.dir.clone(),
            modules: modules.clone(),
            clock: Clock::new(),
            next_hotkey: AtomicU64::new(1),
        });
        let lua = sandbox::new_state(&ctx)
            .and_then(|lua| stdlib::install(&lua, &ctx).map(|()| lua))
            .map_err(|e| format!("cannot set up Lua: {e}"))?;
        let body =
            sandbox::compile(&lua, &spec.source, &spec.caller.plugin).map_err(|e| describe(&e))?;
        let budget = env.limits.init_budget_ms;
        budgeted(ctx.clock.clone(), budget, body.call_async::<()>(()))
            .await
            .map_err(|e| describe(&e))?;
        if let Some(init) = global_fn(&lua, "init") {
            budgeted(ctx.clock.clone(), budget, init.call_async::<()>(()))
                .await
                .map_err(|e| format!("init: {}", describe(&e)))?;
        }
        Ok(Self { lua, ctx })
    }

    /// The coroutine that handles `event`, if a callback is registered for
    /// it. A timer callback receives the timer id, a hotkey callback (press
    /// or release) the binding id; `bricolo.ui` callbacks are described in docs/scripts.md.
    fn callback(&self, event: &ProviderEvent, budget_ms: u64) -> mlua::Result<Option<Call>> {
        let callbacks: Table = self.lua.named_registry_value(CALLBACKS)?;
        let lua = &self.lua;
        let (func, args): (Option<Function>, MultiValue) = match event {
            ProviderEvent::Timer(id) => {
                let once = format!("a{id}");
                let func = match callbacks.get::<Option<Function>>(once.as_str())? {
                    Some(f) => {
                        callbacks.set(once, Nil)?;
                        Some(f)
                    }
                    None => callbacks.get(format!("e{id}"))?,
                };
                (func, (*id as i64).into_lua_multi(lua)?)
            }
            ProviderEvent::Hotkey(id) => (
                callbacks.get(id.as_str())?,
                id.as_str().into_lua_multi(lua)?,
            ),
            ProviderEvent::HotkeyReleased(id) => (
                callbacks.get(stdlib::release_key(id))?,
                id.as_str().into_lua_multi(lua)?,
            ),
            ProviderEvent::Ui(e) => ui_callback(lua, &callbacks, e)?,
            ProviderEvent::App(e) => (
                callbacks.get(stdlib::APP_WATCH)?,
                stdlib::AsTable(e.clone()).into_lua_multi(lua)?,
            ),
            ProviderEvent::System(e) => (
                callbacks.get(stdlib::SYSTEM_WATCH)?,
                stdlib::system_event_name(*e).into_lua_multi(lua)?,
            ),
            ProviderEvent::File(e) => (
                callbacks.get(stdlib::file_key(e.watch))?,
                stdlib::AsTable(e.clone()).into_lua_multi(lua)?,
            ),
        };
        Ok(func.map(|f| -> Call {
            Box::pin(budgeted(
                self.ctx.clock.clone(),
                budget_ms,
                f.call_async::<()>(args),
            ))
        }))
    }

    async fn dispose(&self, budget_ms: u64) -> Result<(), String> {
        let Some(dispose) = global_fn(&self.lua, "dispose") else {
            return Ok(());
        };
        budgeted(
            self.ctx.clock.clone(),
            budget_ms,
            dispose.call_async::<()>(()),
        )
        .await
        .map_err(|e| format!("dispose: {}", describe(&e)))
    }
}

/// `on_switch(id, on)`, `on_button(id)`, `on_choice(index, label)` with a
/// 1-based index (nil, nil when dismissed), `on_review(accepted, text)`. A
/// panel's callbacks go once it is answered.
fn ui_callback(
    lua: &Lua,
    callbacks: &Table,
    event: &UiEvent,
) -> mlua::Result<(Option<Function>, MultiValue)> {
    let once = |id: &str| -> mlua::Result<Option<Function>> {
        let func = callbacks.get(ui_panel_key(id))?;
        callbacks.set(ui_panel_key(id), Nil)?;
        Ok(func)
    };
    Ok(match event {
        UiEvent::Switch { id, on } => (
            callbacks.get(UI_SWITCH)?,
            (id.as_str(), *on).into_lua_multi(lua)?,
        ),
        UiEvent::Button { id } => (callbacks.get(UI_BUTTON)?, id.as_str().into_lua_multi(lua)?),
        UiEvent::Choice { panel, index } => {
            let choices: Option<Table> = callbacks.get(ui_choices_key(panel))?;
            callbacks.set(ui_choices_key(panel), Nil)?;
            let label = match (index, &choices) {
                (Some(i), Some(list)) => list.get::<Value>(i + 1)?,
                _ => Nil,
            };
            let index = index.map_or(Nil, |i| Value::Integer(i64::from(i) + 1));
            (once(panel)?, (index, label).into_lua_multi(lua)?)
        }
        UiEvent::Review {
            panel,
            accepted,
            text,
        } => (
            once(panel)?,
            (*accepted, text.as_str()).into_lua_multi(lua)?,
        ),
    })
}

/// The handle the tier keeps for a running script.
pub struct Running {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Running {
    /// Cancels the callbacks in flight, calls `dispose()` (up to
    /// [`DRAIN`]) and waits for the task to end.
    pub async fn stop(self) {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Cmd::Stop(reply)).is_ok() {
            let _ = done.await;
        }
    }
}

/// Starts the task that owns `live`. `tx` and `rx` are the script's queue,
/// already in the mailbox so no event fired during start is lost.
pub fn spawn(
    env: Arc<Env>,
    spec: Arc<Spec>,
    live: Live,
    info: SharedInfo,
    modules: Modules,
    tx: mpsc::UnboundedSender<Cmd>,
    rx: mpsc::UnboundedReceiver<Cmd>,
) -> Running {
    tokio::spawn(run(env, spec, live, info, modules, rx));
    Running { tx }
}

async fn run(
    env: Arc<Env>,
    spec: Arc<Spec>,
    mut live: Live,
    info: SharedInfo,
    modules: Modules,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
) {
    let name = spec.caller.plugin.as_str();
    let budget = env.limits.event_budget_ms;
    // Callback coroutines in flight. They take turns on this task, so one
    // Lua state never runs two at once, but one waiting on a provider does
    // not hold up the others.
    let mut calls: FuturesUnordered<Call> = FuturesUnordered::new();
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(Cmd::Event(event)) => match live.callback(&event, budget) {
                    Ok(Some(call)) => calls.push(call),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(plugin = name, "cannot dispatch {event:?}: {e}"),
                },
                Some(Cmd::Stop(reply)) => {
                    calls.clear();
                    match tokio::time::timeout(DRAIN, live.dispose(budget)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!(plugin = name, "{e}"),
                        Err(_) => tracing::warn!(plugin = name, "dispose cut off"),
                    }
                    env.providers.release(name);
                    let _ = reply.send(());
                    return;
                }
                None => {
                    env.providers.release(name);
                    return;
                }
            },
            Some(result) = calls.next(), if !calls.is_empty() => {
                let Err(e) = result else { continue };
                let reason = describe(&e);
                tracing::error!(plugin = name, "{reason}");
                calls.clear();
                env.providers.release(name);
                let first = {
                    let mut i = info.lock().unwrap();
                    i.error = Some(first_line(&reason));
                    i.restarts == 0
                };
                if !first {
                    set_off(&info, reason);
                    return;
                }
                info.lock().unwrap().restarts = 1;
                tracing::warn!(plugin = name, "restarting once");
                match Live::start(&env, &spec, &modules).await {
                    Ok(fresh) => live = fresh,
                    Err(e) => {
                        tracing::error!(plugin = name, "restart failed: {e}");
                        env.providers.release(name);
                        set_off(&info, e);
                        return;
                    }
                }
            }
        }
    }
}

/// Marks the script off. The status keeps the first line of `reason`; the
/// log has the whole traceback.
pub fn set_off(info: &SharedInfo, reason: String) {
    let mut i = info.lock().unwrap();
    i.state = State::Off;
    i.error = Some(first_line(&reason));
}

fn first_line(reason: &str) -> String {
    reason.lines().next().unwrap_or_default().to_string()
}
