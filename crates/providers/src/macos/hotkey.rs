//! `hotkey` on macOS: global-hotkey registrations on the main-thread loop,
//! and the Fn key through an event tap (`fn_key`). A press or a release
//! finds its binding by the OS id and goes to the owning plugin's queue
//! through the event sink.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};

use super::fn_key;
use super::main_loop::MainHandle;
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent};
use crate::hotkey::{Chord, HotkeyProvider, Modifier};

struct Binding {
    plugin: String,
    id: String,
    chord: String,
    /// None for Fn, which has no Carbon hotkey.
    hotkey: Option<HotKey>,
}

/// Bindings by the OS id of their hotkey.
type Table = Arc<Mutex<HashMap<u32, Binding>>>;

pub struct MacHotkeys {
    main: MainHandle,
    table: Table,
}

impl MacHotkeys {
    pub fn new(main: MainHandle, sink: EventSink) -> Self {
        let table: Table = Arc::default();
        let presses = table.clone();
        main.on_press(Arc::new(move |os_id, pressed| {
            let target = presses
                .lock()
                .unwrap()
                .get(&os_id)
                .map(|b| (b.plugin.clone(), b.id.clone()));
            if let Some((plugin, id)) = target {
                let event = if pressed {
                    ProviderEvent::Hotkey(id)
                } else {
                    ProviderEvent::HotkeyReleased(id)
                };
                sink(&plugin, event);
            }
        }));
        Self { main, table }
    }

    fn find(&self, plugin: &str, id: &str) -> Option<u32> {
        self.table
            .lock()
            .unwrap()
            .iter()
            .find(|(_, b)| b.plugin == plugin && b.id == id)
            .map(|(os, _)| *os)
    }
}

fn to_hotkey(chord: &Chord) -> Result<HotKey, CallError> {
    let mut mods = Modifiers::empty();
    for m in &chord.mods {
        mods |= match m {
            Modifier::Cmd => Modifiers::META,
            Modifier::Ctrl => Modifiers::CONTROL,
            Modifier::Alt => Modifiers::ALT,
            Modifier::Shift => Modifiers::SHIFT,
        };
    }
    let code = Code::from_str(&chord.code)
        .map_err(|_| CallError::Failed(format!("unknown key {}", chord.code)))?;
    Ok(HotKey::new(Some(mods), code))
}

#[async_trait]
impl HotkeyProvider for MacHotkeys {
    async fn bind(&self, caller: &Caller, id: String, chord: String) -> Result<(), CallError> {
        let parsed = Chord::parse(&chord).map_err(CallError::Failed)?;
        let hotkey = if parsed.is_fn() {
            None
        } else {
            Some(to_hotkey(&parsed)?)
        };
        let os_id = hotkey.map_or(fn_key::OS_ID, |h| h.id());
        let owner = self
            .table
            .lock()
            .unwrap()
            .get(&os_id)
            .map(|b| (b.plugin.clone(), b.id.clone()));
        match owner {
            Some((plugin, bound)) if plugin == caller.plugin && bound == id => return Ok(()),
            Some((plugin, bound)) => {
                return Err(CallError::Failed(format!(
                    "{chord} is already bound by {plugin} as {bound}"
                )));
            }
            None => {}
        }
        // Rebinding an id to a new chord drops the old chord.
        self.unbind(caller, id.clone()).await?;
        match hotkey {
            Some(hotkey) => self
                .main
                .register(hotkey)
                .await
                .map_err(|e| CallError::Failed(format!("cannot bind {chord}: {e}")))?,
            None => self.main.watch_fn(true).await.map_err(|e| {
                tracing::warn!("cannot bind {chord}: {e}");
                CallError::Denied
            })?,
        }
        self.table.lock().unwrap().insert(
            os_id,
            Binding {
                plugin: caller.plugin.clone(),
                id,
                chord,
                hotkey,
            },
        );
        Ok(())
    }

    async fn unbind(&self, caller: &Caller, id: String) -> Result<(), CallError> {
        let Some(os) = self.find(&caller.plugin, &id) else {
            return Ok(());
        };
        let Some(binding) = self.table.lock().unwrap().remove(&os) else {
            return Ok(());
        };
        match binding.hotkey {
            Some(hotkey) => self.main.unregister(hotkey).await,
            None => self.main.watch_fn(false).await,
        }
        .map_err(CallError::Failed)
    }

    fn release(&self, plugin: &str) {
        let mut table = self.table.lock().unwrap();
        table.retain(|_, b| {
            let keep = b.plugin != plugin;
            if !keep {
                match b.hotkey {
                    Some(hotkey) => self.main.unregister_later(hotkey),
                    None => self.main.unwatch_fn_later(),
                }
            }
            keep
        });
    }

    fn bound(&self, plugin: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .table
            .lock()
            .unwrap()
            .values()
            .filter(|b| b.plugin == plugin)
            .map(|b| (b.id.clone(), b.chord.clone()))
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_map_to_global_hotkey_codes() {
        let hk = to_hotkey(&Chord::parse("cmd+alt+left").unwrap()).unwrap();
        let same = HotKey::new(Some(Modifiers::META | Modifiers::ALT), Code::ArrowLeft);
        assert_eq!(hk.id(), same.id());
        for key in [
            "a",
            "7",
            "f20",
            "space",
            "/",
            "`",
            "pagedown",
            "forwarddelete",
        ] {
            let chord = Chord::parse(&format!("ctrl+{key}")).unwrap();
            assert!(to_hotkey(&chord).is_ok(), "{key}");
        }
    }
}
