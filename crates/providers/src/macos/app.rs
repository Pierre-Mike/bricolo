//! `app` on macOS: `open` to launch apps and open URLs, NSWorkspace to see
//! what runs.

use async_trait::async_trait;
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey,
    NSWorkspaceDidActivateApplicationNotification, NSWorkspaceDidLaunchApplicationNotification,
    NSWorkspaceDidTerminateApplicationNotification,
};
use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFBoolean, CFString};
use objc2_foundation::NSNotification;

use super::events::{Watchers, names, observe, toggle};
use super::window::AxErr;
use crate::app::{AppInfo, AppProvider, check_app, check_url, launch_args};
use crate::caller::Caller;
use crate::error::CallError;
use crate::event::{AppChange, AppEvent, EventSink, ProviderEvent};

pub struct MacApps {
    /// Present when the node runs the main-thread loop, which delivers the
    /// NSWorkspace notifications.
    watchers: Option<Watchers>,
}

impl MacApps {
    /// With a sink, `watch` works: NSWorkspace's launch, terminate and
    /// activate notifications go to the watching plugins.
    pub fn new(sink: Option<EventSink>) -> Self {
        let Some(sink) = sink else {
            return Self { watchers: None };
        };
        let watchers = Watchers::default();
        objc2::rc::autoreleasepool(|_| {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            // SAFETY: reading AppKit constants.
            let kinds = unsafe {
                [
                    (
                        NSWorkspaceDidLaunchApplicationNotification,
                        AppChange::Launched,
                    ),
                    (
                        NSWorkspaceDidTerminateApplicationNotification,
                        AppChange::Terminated,
                    ),
                    (
                        NSWorkspaceDidActivateApplicationNotification,
                        AppChange::Activated,
                    ),
                ]
            };
            for (name, change) in kinds {
                let (sink, watchers) = (sink.clone(), watchers.clone());
                observe(&center, name, move |n| {
                    let Some(app) = notified_app(n) else { return };
                    for plugin in names(&watchers) {
                        let event = AppEvent {
                            change,
                            app: app.clone(),
                        };
                        sink(&plugin, ProviderEvent::App(event));
                    }
                });
            }
        });
        Self {
            watchers: Some(watchers),
        }
    }
}

/// The app a workspace notification is about.
fn notified_app(n: &NSNotification) -> Option<AppInfo> {
    let user_info = n.userInfo()?;
    // SAFETY: reading an AppKit constant.
    let value = user_info.objectForKey(unsafe { NSWorkspaceApplicationKey })?;
    let app = value.downcast_ref::<NSRunningApplication>()?;
    Some(info(app))
}

async fn open(args: Vec<String>) -> Result<(), CallError> {
    let out = tokio::process::Command::new("/usr/bin/open")
        .args(&args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(CallError::failed)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(CallError::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}

/// The running app whose bundle id or name is `app`.
fn find(app: &str) -> Result<objc2::rc::Retained<NSRunningApplication>, CallError> {
    let app = check_app(app)?;
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .find(|a| {
            a.bundleIdentifier().is_some_and(|b| b.to_string() == app)
                || a.localizedName().is_some_and(|n| n.to_string() == app)
        })
        .ok_or_else(|| CallError::Failed("no such app".into()))
}

fn info(app: &NSRunningApplication) -> AppInfo {
    AppInfo {
        name: app
            .localizedName()
            .map(|s| s.to_string())
            .unwrap_or_default(),
        bundle_id: app.bundleIdentifier().map(|s| s.to_string()),
        pid: app.processIdentifier(),
    }
}

#[async_trait]
impl AppProvider for MacApps {
    async fn launch(&self, _: &Caller, app: String) -> Result<(), CallError> {
        open(launch_args(&app)?).await
    }

    async fn open_url(&self, _: &Caller, url: String) -> Result<(), CallError> {
        check_url(&url)?;
        open(vec![url]).await
    }

    async fn running(&self, _: &Caller) -> Result<Vec<AppInfo>, CallError> {
        tokio::task::spawn_blocking(|| {
            objc2::rc::autoreleasepool(|_| {
                NSWorkspace::sharedWorkspace()
                    .runningApplications()
                    .iter()
                    .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular)
                    .map(|a| info(&a))
                    .collect()
            })
        })
        .await
        .map_err(CallError::failed)
    }

    async fn frontmost(&self, _: &Caller) -> Result<Option<AppInfo>, CallError> {
        tokio::task::spawn_blocking(|| {
            objc2::rc::autoreleasepool(|_| {
                NSWorkspace::sharedWorkspace()
                    .frontmostApplication()
                    .map(|a| info(&a))
            })
        })
        .await
        .map_err(CallError::failed)
    }

    async fn watch(&self, caller: &Caller, on: bool) -> Result<(), CallError> {
        let watchers = self.watchers.as_ref().ok_or(CallError::Unavailable)?;
        toggle(watchers, &caller.plugin, on);
        Ok(())
    }

    fn release(&self, plugin: &str) {
        if let Some(watchers) = &self.watchers {
            toggle(watchers, plugin, false);
        }
    }

    async fn quit(&self, _: &Caller, app: String) -> Result<(), CallError> {
        tokio::task::spawn_blocking(move || {
            objc2::rc::autoreleasepool(|_| {
                let app = find(&app)?;
                if app.terminate() {
                    Ok(())
                } else {
                    Err(CallError::Failed(
                        "the app did not accept the request".into(),
                    ))
                }
            })
        })
        .await
        .map_err(CallError::failed)?
    }

    async fn set_hidden(&self, _: &Caller, app: String, hidden: bool) -> Result<(), CallError> {
        tokio::task::spawn_blocking(move || {
            objc2::rc::autoreleasepool(|_| {
                let app = find(&app)?;
                // NSRunningApplication.hide() answers NO from the node, so
                // set AXHidden on the application element instead.
                let element = unsafe { AXUIElement::new_application(app.processIdentifier()) };
                let value = CFBoolean::new(hidden);
                let err =
                    unsafe { element.set_attribute_value(&CFString::from_str("AXHidden"), value) };
                match super::window::check(err) {
                    Ok(()) => Ok(()),
                    Err(AxErr::Denied) => Err(CallError::Denied),
                    Err(_) => Err(CallError::Failed(
                        "the app did not accept the request".into(),
                    )),
                }
            })
        })
        .await
        .map_err(CallError::failed)?
    }
}
