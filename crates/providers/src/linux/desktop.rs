//! The Linux `window`, `hotkey` and `keyboard` providers the registry
//! uses. Each call goes to Hyprland when the node runs under it
//! ([`Hyprland::current`]), else to the X11 backend, which answers
//! `unavailable` on other Wayland compositors and with no display.

use std::sync::Arc;

use async_trait::async_trait;

use super::hotkey::X11Hotkeys;
use super::hyprland::{Hyprland, hotkey::HyprHotkeys, keyboard::HyprKeyboard, window as hw};
use super::keyboard::X11Keyboard;
use super::window::X11Windows;
use super::x11::X11;
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::EventSink;
use crate::hotkey::HotkeyProvider;
use crate::keyboard::KeyboardProvider;
use crate::window::{Rect, Screen, WindowInfo, WindowProvider};

pub struct LinuxWindows {
    x11: X11Windows,
}

impl LinuxWindows {
    pub fn new(x: Arc<X11>) -> Self {
        Self {
            x11: X11Windows::new(x),
        }
    }
}

#[async_trait]
impl WindowProvider for LinuxWindows {
    async fn list(&self, caller: &Caller) -> Result<Vec<WindowInfo>, CallError> {
        match Hyprland::current() {
            Some(h) => hw::blocking(h, hw::list).await,
            None => self.x11.list(caller).await,
        }
    }

    async fn focused(&self, caller: &Caller) -> Result<Option<WindowInfo>, CallError> {
        match Hyprland::current() {
            Some(h) => hw::blocking(h, hw::focused).await,
            None => self.x11.focused(caller).await,
        }
    }

    async fn focus(&self, caller: &Caller, id: u64) -> Result<(), CallError> {
        match Hyprland::current() {
            Some(h) => hw::blocking(h, move |h| hw::focus(h, id)).await,
            None => self.x11.focus(caller, id).await,
        }
    }

    async fn set_frame(&self, caller: &Caller, id: u64, frame: Rect) -> Result<(), CallError> {
        match Hyprland::current() {
            Some(h) => hw::blocking(h, move |h| hw::set_frame(h, id, frame)).await,
            None => self.x11.set_frame(caller, id, frame).await,
        }
    }

    async fn close(&self, caller: &Caller, id: u64) -> Result<(), CallError> {
        match Hyprland::current() {
            Some(h) => hw::blocking(h, move |h| hw::close(h, id)).await,
            None => self.x11.close(caller, id).await,
        }
    }

    async fn set_minimized(
        &self,
        caller: &Caller,
        id: u64,
        minimized: bool,
    ) -> Result<(), CallError> {
        match Hyprland::current() {
            Some(h) => hw::blocking(h, move |h| hw::set_minimized(h, id, minimized)).await,
            None => self.x11.set_minimized(caller, id, minimized).await,
        }
    }

    async fn screens(&self, caller: &Caller) -> Result<Vec<Screen>, CallError> {
        match Hyprland::current() {
            Some(h) => hw::blocking(h, hw::screens).await,
            None => self.x11.screens(caller).await,
        }
    }
}

pub struct LinuxHotkeys {
    x11: X11Hotkeys,
    hypr: HyprHotkeys,
}

impl LinuxHotkeys {
    pub fn new(sink: EventSink) -> Self {
        Self {
            x11: X11Hotkeys::new(sink.clone()),
            hypr: HyprHotkeys::new(sink),
        }
    }
}

#[async_trait]
impl HotkeyProvider for LinuxHotkeys {
    async fn bind(&self, caller: &Caller, id: String, chord: String) -> Result<(), CallError> {
        match Hyprland::current() {
            Some(h) => self.hypr.bind(h, caller, id, chord).await,
            None => self.x11.bind(caller, id, chord).await,
        }
    }

    async fn unbind(&self, caller: &Caller, id: String) -> Result<(), CallError> {
        if self.hypr.holds(&caller.plugin, &id) {
            if let Some(h) = Hyprland::current() {
                self.hypr.unbind(h, &caller.plugin, &id).await;
                return Ok(());
            }
            // Hyprland went away: forget the binding all the same.
            self.hypr.release_one(&caller.plugin, &id);
            return Ok(());
        }
        self.x11.unbind(caller, id).await
    }

    fn release(&self, plugin: &str) {
        self.hypr.release(plugin);
        self.x11.release(plugin);
    }

    fn bound(&self, plugin: &str) -> Vec<(String, String)> {
        let mut out = self.hypr.bound(plugin);
        out.extend(self.x11.bound(plugin));
        out.sort();
        out
    }
}

#[derive(Default)]
pub struct LinuxKeyboard {
    x11: X11Keyboard,
    hypr: HyprKeyboard,
}

#[async_trait]
impl KeyboardProvider for LinuxKeyboard {
    async fn keystroke(
        &self,
        caller: &Caller,
        mods: Vec<String>,
        key: String,
    ) -> Result<(), CallError> {
        match Hyprland::current() {
            Some(h) => self.hypr.keystroke(h, mods, key).await,
            None => self.x11.keystroke(caller, mods, key).await,
        }
    }

    async fn type_text(&self, caller: &Caller, text: String) -> Result<(), CallError> {
        match Hyprland::current() {
            Some(h) => self.hypr.type_text(h, text).await,
            None => self.x11.type_text(caller, text).await,
        }
    }
}
