//! One running plugin: its store, and the task that feeds it one event at
//! a time, restarts it once after a failure, and disposes it on stop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bricolo_providers::{Caller, Providers};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};
use wasmtime::{Store, Trap};

use crate::engine::{Checked, WasmEngine, ticks};
use crate::host::{Bindings, Event, HostState};
use crate::node_config::RuntimeLimits;

/// How long an in-flight event may finish after a stop or reload.
pub const DRAIN: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    /// Failed its checks: not loaded at all.
    Refused,
    /// Failed twice, or failed in `init`. Stays off until reloaded.
    Off,
    /// Disposed on purpose: shutdown or uninstall.
    Stopped,
    /// Turned off in the hub. Stays off, across restarts, until turned on.
    Disabled,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub state: State,
    /// Why it is refused or off, or why the last reload was refused.
    pub error: Option<String>,
    /// Restarts since the last load: 0 or 1.
    pub restarts: u32,
}

pub type SharedInfo = Arc<Mutex<PluginInfo>>;

/// Everything needed to start, or restart, one instance.
pub struct Spec {
    pub name: String,
    pub version: String,
    pub checked: Checked,
    pub caller: Caller,
    pub config: String,
    pub data_dir: PathBuf,
    pub limits: RuntimeLimits,
    pub dependencies: Vec<String>,
}

pub enum Cmd {
    Event(Event),
    Stop(oneshot::Sender<()>),
}

/// A live instance: its store and bindings.
pub struct Live {
    store: Store<HostState>,
    bindings: Bindings,
    event_ticks: u64,
    budget_ms: u64,
}

fn describe(e: &wasmtime::Error, what: &str, budget_ms: u64) -> String {
    match e.downcast_ref::<Trap>() {
        Some(Trap::Interrupt) => format!("{what} ran past its {budget_ms} ms budget"),
        _ => format!("trapped in {what}: {e:#}"),
    }
}

impl Live {
    /// Instantiates the component and runs `init`.
    pub async fn start(
        engine: &WasmEngine,
        providers: &Providers,
        spec: &Spec,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&spec.data_dir)
            .map_err(|e| format!("cannot create {}: {e}", spec.data_dir.display()))?;
        let event_ticks = ticks(spec.limits.event_budget_ms);
        let state = HostState::new(
            spec.caller.clone(),
            providers.clone(),
            &spec.data_dir,
            event_ticks,
        );
        let mut store = Store::new(&engine.engine, state);
        let init_ms = spec.limits.init_budget_ms;
        store.set_epoch_deadline(ticks(init_ms));
        let bindings = spec
            .checked
            .pre
            .instantiate_async(&mut store)
            .await
            .map_err(|e| describe(&e, "instantiation", init_ms))?;
        // Host calls during init restart the event budget; init keeps its own.
        store.data_mut().budget_ticks = ticks(init_ms);
        let config = spec.config.clone();
        let result = store
            .run_concurrent(async |acc| bindings.call_init(acc, config).await)
            .await
            .and_then(|r| r);
        store.data_mut().budget_ticks = event_ticks;
        match result {
            Ok(Ok(())) => Ok(Self {
                store,
                bindings,
                event_ticks,
                budget_ms: spec.limits.event_budget_ms,
            }),
            Ok(Err(msg)) => Err(format!("init returned an error: {msg}")),
            Err(e) => Err(describe(&e, "init", init_ms)),
        }
    }

    pub async fn on_event(&mut self, event: Event) -> Result<(), String> {
        self.store.set_epoch_deadline(self.event_ticks);
        let bindings = &self.bindings;
        self.store
            .run_concurrent(async |acc| bindings.call_on_event(acc, event).await)
            .await
            .and_then(|r| r)
            .map_err(|e| describe(&e, "on-event", self.budget_ms))
    }

    pub async fn dispose(&mut self) -> Result<(), String> {
        self.store.set_epoch_deadline(self.event_ticks);
        let bindings = &self.bindings;
        self.store
            .run_concurrent(async |acc| bindings.call_dispose(acc).await)
            .await
            .and_then(|r| r)
            .map_err(|e| describe(&e, "dispose", self.budget_ms))
    }
}

/// The handle the runtime keeps for a running plugin.
pub struct Running {
    pub tx: mpsc::UnboundedSender<Cmd>,
    stopping: watch::Sender<bool>,
}

impl Running {
    /// Lets the current event finish (up to [`DRAIN`]), drops queued
    /// events, calls `dispose` and waits for the task to end.
    pub async fn stop(self) {
        let _ = self.stopping.send(true);
        let (reply, done) = oneshot::channel();
        if self.tx.send(Cmd::Stop(reply)).is_ok() {
            let _ = done.await;
        }
    }
}

pub struct Env {
    pub engine: WasmEngine,
    pub providers: Providers,
}

/// Starts the task that owns `live`.
pub fn spawn(env: Arc<Env>, spec: Arc<Spec>, live: Live, info: SharedInfo) -> Running {
    let (tx, rx) = mpsc::unbounded_channel();
    let (stopping, stop_rx) = watch::channel(false);
    tokio::spawn(run(env, spec, live, info, rx, stop_rx));
    Running { tx, stopping }
}

async fn run(
    env: Arc<Env>,
    spec: Arc<Spec>,
    mut live: Live,
    info: SharedInfo,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    mut stopping: watch::Receiver<bool>,
) {
    let name = spec.name.as_str();
    // False once a call was cut off: the store is then in an unknown state.
    let mut usable = true;
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Event(event) => {
                if *stopping.borrow() {
                    continue;
                }
                let result = {
                    let call = live.on_event(event);
                    tokio::pin!(call);
                    tokio::select! {
                        r = &mut call => Some(r),
                        _ = stopping.changed() => tokio::time::timeout(DRAIN, &mut call).await.ok(),
                    }
                };
                match result {
                    Some(Ok(())) => {}
                    None => {
                        tracing::warn!(plugin = name, "event cut off after {} s", DRAIN.as_secs());
                        usable = false;
                    }
                    Some(Err(reason)) => {
                        tracing::error!(plugin = name, "{reason}");
                        env.providers.release(name);
                        let first = {
                            let mut i = info.lock().unwrap();
                            i.error = Some(reason.clone());
                            i.restarts == 0
                        };
                        if !first {
                            set_off(&info, reason);
                            return;
                        }
                        info.lock().unwrap().restarts = 1;
                        tracing::warn!(plugin = name, "restarting once");
                        match Live::start(&env.engine, &env.providers, &spec).await {
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
            Cmd::Stop(reply) => {
                if usable {
                    match tokio::time::timeout(DRAIN, live.dispose()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::warn!(plugin = name, "{e}"),
                        Err(_) => tracing::warn!(plugin = name, "dispose cut off"),
                    }
                }
                env.providers.release(name);
                let _ = reply.send(());
                return;
            }
        }
    }
}

fn set_off(info: &SharedInfo, reason: String) {
    let mut i = info.lock().unwrap();
    i.state = State::Off;
    i.error = Some(reason);
}
