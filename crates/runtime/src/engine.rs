//! The shared Wasmtime engine and linker, and the checks a component must
//! pass before any of its code runs.

use std::sync::Arc;
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Config, Engine};

use crate::host::{Bindings, BindingsPre, HostState};
use crate::manifest::{Manifest, check_import};

/// The epoch advances every `TICK`. Budgets are counted in ticks.
pub const TICK: Duration = Duration::from_millis(10);

pub fn ticks(ms: u64) -> u64 {
    (ms / TICK.as_millis() as u64).max(1)
}

#[derive(Clone)]
pub struct WasmEngine {
    pub engine: Engine,
    pub linker: Arc<Linker<HostState>>,
}

/// A component that passed every check, ready to instantiate.
#[derive(Clone)]
pub struct Checked {
    pub pre: BindingsPre<HostState>,
    pub imports: Vec<String>,
}

impl WasmEngine {
    /// Builds the engine and starts the thread that advances its epoch.
    /// The thread stops once the engine is dropped.
    pub fn new() -> Result<Self, String> {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| format!("{e:#}"))?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| format!("{e:#}"))?;
        Bindings::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |s| s)
            .map_err(|e| format!("{e:#}"))?;
        let weak = engine.weak();
        std::thread::Builder::new()
            .name("bricolo-epoch".into())
            .spawn(move || {
                while let Some(engine) = weak.upgrade() {
                    engine.increment_epoch();
                    drop(engine);
                    std::thread::sleep(TICK);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            engine,
            linker: Arc::new(linker),
        })
    }

    /// Compiles `wasm` and checks it against `manifest` without running it:
    /// every import must be WASI or a standard library interface the
    /// manifest lists, and the exports must match the `plugin` world.
    pub fn check(&self, manifest: &Manifest, wasm: &[u8]) -> Result<Checked, String> {
        manifest.validate()?;
        let component = Component::new(&self.engine, wasm)
            .map_err(|e| format!("not a valid component: {e:#}"))?;
        let ty = component.component_type();
        let imports: Vec<String> = ty
            .imports(&self.engine)
            .map(|(n, _)| n.to_string())
            .collect();
        let problems: Vec<String> = imports
            .iter()
            .filter_map(|name| check_import(name, manifest).err())
            .collect();
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        let pre = self
            .linker
            .instantiate_pre(&component)
            .map_err(|e| format!("cannot link: {e:#}"))?;
        let pre = BindingsPre::new(pre)
            .map_err(|e| format!("exports do not match the bricolo:stdlib plugin world: {e:#}"))?;
        Ok(Checked { pre, imports })
    }
}
