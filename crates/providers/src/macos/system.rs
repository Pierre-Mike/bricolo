//! `system` on macOS: `ioreg` for idle time, `pmset` for power, `caffeinate`
//! for keep-awake, and NSWorkspace, distributed and application
//! notifications for system events. Built only with a `Desktop`.

use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use async_trait::async_trait;
use objc2_app_kit::{NSApplicationDidChangeScreenParametersNotification, NSWorkspace};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotificationCenter, NSString};

use super::events::{Watchers, names, observe, toggle};
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{EventSink, ProviderEvent, SystemEvent};
use crate::system::{Power, SystemProvider, parse_hid_idle, parse_pmset};

pub struct MacSystem {
    watchers: Watchers,
    awake: Mutex<HashMap<String, Child>>,
}

impl MacSystem {
    /// Starts observing; events reach the plugins that called `watch`.
    pub fn new(sink: EventSink) -> Self {
        let watchers = Watchers::default();
        let forward = |event: SystemEvent| {
            let (sink, watchers) = (sink.clone(), watchers.clone());
            move |_: &_| {
                for plugin in names(&watchers) {
                    sink(&plugin, ProviderEvent::System(event));
                }
            }
        };
        objc2::rc::autoreleasepool(|_| {
            let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
            let name = NSString::from_str("NSWorkspaceWillSleepNotification");
            observe(&workspace, &name, forward(SystemEvent::WillSleep));
            let name = NSString::from_str("NSWorkspaceDidWakeNotification");
            observe(&workspace, &name, forward(SystemEvent::DidWake));
            let distributed = NSDistributedNotificationCenter::defaultCenter();
            let name = NSString::from_str("com.apple.screenIsLocked");
            observe(&distributed, &name, forward(SystemEvent::ScreenLocked));
            let name = NSString::from_str("com.apple.screenIsUnlocked");
            observe(&distributed, &name, forward(SystemEvent::ScreenUnlocked));
            let default = NSNotificationCenter::defaultCenter();
            // SAFETY: reading an AppKit constant.
            let name = unsafe { NSApplicationDidChangeScreenParametersNotification };
            observe(&default, name, forward(SystemEvent::ScreensChanged));
        });
        Self {
            watchers,
            awake: Mutex::default(),
        }
    }
}

async fn run(program: &str, args: &[&str]) -> Result<String, CallError> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(CallError::failed)?;
    if !out.status.success() {
        return Err(CallError::Failed(format!(
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn stop(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[async_trait]
impl SystemProvider for MacSystem {
    async fn idle_seconds(&self, _: &Caller) -> Result<u64, CallError> {
        let text = run("/usr/sbin/ioreg", &["-c", "IOHIDSystem"]).await?;
        parse_hid_idle(&text).ok_or_else(|| CallError::Failed("ioreg reported no idle time".into()))
    }

    async fn power(&self, _: &Caller) -> Result<Power, CallError> {
        Ok(parse_pmset(&run("/usr/bin/pmset", &["-g", "batt"]).await?))
    }

    async fn keep_awake(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        let mut awake = self.awake.lock().unwrap();
        if on {
            if awake.contains_key(&caller.plugin) {
                return Ok(());
            }
            let child = Command::new("/usr/bin/caffeinate")
                .arg("-i")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(CallError::failed)?;
            awake.insert(caller.plugin.clone(), child);
        } else if let Some(child) = awake.remove(&caller.plugin) {
            stop(child);
        }
        Ok(())
    }

    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        toggle(&self.watchers, &caller.plugin, on);
        Ok(())
    }

    fn release(&self, plugin: &str) {
        toggle(&self.watchers, plugin, false);
        if let Some(child) = self.awake.lock().unwrap().remove(plugin) {
            stop(child);
        }
    }
}

impl Drop for MacSystem {
    fn drop(&mut self) {
        for (_, child) in self.awake.lock().unwrap().drain() {
            stop(child);
        }
    }
}
