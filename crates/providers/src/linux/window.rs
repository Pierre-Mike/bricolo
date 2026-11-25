//! `window` on X11, through the window manager's EWMH properties. Needs an
//! EWMH window manager (openbox, xfwm4, KWin and Mutter on X11 all are);
//! without one the window list is empty.

use std::sync::Arc;

use async_trait::async_trait;

use super::x11::{X11, XError};
use crate::caller::Caller;
use crate::error::CallError;
use crate::window::{Rect, Screen, WindowInfo, WindowProvider};

pub struct X11Windows {
    x: Arc<X11>,
}

impl X11Windows {
    pub fn new(x: Arc<X11>) -> Self {
        Self { x }
    }
}

#[async_trait]
impl WindowProvider for X11Windows {
    async fn list(&self, _: &Caller) -> Result<Vec<WindowInfo>, CallError> {
        self.x
            .with(|c| {
                let mut out = Vec::new();
                for window in c.clients()? {
                    // A window can close while we read it.
                    match c.is_app_window(window).and_then(|app| {
                        if app {
                            c.info(window).map(Some)
                        } else {
                            Ok(None)
                        }
                    }) {
                        Ok(Some(info)) => out.push(info),
                        Ok(None) | Err(XError::NotFound) => {}
                        Err(e) => return Err(e),
                    }
                }
                Ok(out)
            })
            .await
    }

    async fn focused(&self, _: &Caller) -> Result<Option<WindowInfo>, CallError> {
        self.x
            .with(|c| match c.active()? {
                Some(window) => match c.info(window) {
                    Ok(info) => Ok(Some(info)),
                    Err(XError::NotFound) => Ok(None),
                    Err(e) => Err(e),
                },
                None => Ok(None),
            })
            .await
    }

    async fn focus(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        self.x.with(move |c| c.focus(c.client(id)?)).await
    }

    async fn set_frame(&self, _: &Caller, id: u64, frame: Rect) -> Result<(), CallError> {
        self.x
            .with(move |c| c.set_frame(c.client(id)?, frame))
            .await
    }

    async fn close(&self, _: &Caller, id: u64) -> Result<(), CallError> {
        self.x.with(move |c| c.close(c.client(id)?)).await
    }

    async fn set_minimized(&self, _: &Caller, id: u64, minimized: bool) -> Result<(), CallError> {
        self.x
            .with(move |c| c.set_minimized(c.client(id)?, minimized))
            .await
    }

    async fn screens(&self, _: &Caller) -> Result<Vec<Screen>, CallError> {
        self.x.with(|c| c.screens()).await
    }
}
