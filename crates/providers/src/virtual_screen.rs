//! `virtual-screen`: screens no monitor shows, where apps run and are
//! driven without touching the user's screen. [`VirtualScreens`] keeps
//! who owns which screen, checks `launch` against the caller's `[shell]`
//! grant, and ends a plugin's screens when it stops; an
//! [`ScreenBackend`] starts and drives them: Xvfb on Linux
//! (`linux/virtual_screen.rs`), a hidden desktop on Windows
//! (`windows/virtual_screen.rs`). macOS has none yet: a virtual display
//! needs the private `CGVirtualDisplay` class or a virtual machine, so it
//! answers `unavailable`.
//!
//! Each caller sees and ends only its own screens.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::caller::Caller;
use crate::error::CallError;
use crate::hotkey::Chord;

/// Screens one caller may hold at once.
pub const MAX_PER_CALLER: usize = 4;
pub const MAX_SIDE: u32 = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualScreenInfo {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub pids: Vec<u32>,
}

#[async_trait]
pub trait VirtualScreenProvider: Send + Sync {
    async fn create(
        &self,
        caller: &Caller,
        width: u32,
        height: u32,
    ) -> Result<VirtualScreenInfo, CallError>;
    async fn list(&self, caller: &Caller) -> Result<Vec<VirtualScreenInfo>, CallError>;
    async fn launch(
        &self,
        caller: &Caller,
        id: u32,
        program: String,
        args: Vec<String>,
    ) -> Result<u32, CallError>;
    async fn capture(&self, caller: &Caller, id: u32) -> Result<String, CallError>;
    async fn click(&self, caller: &Caller, id: u32, x: i32, y: i32) -> Result<(), CallError>;
    async fn type_text(&self, caller: &Caller, id: u32, text: String) -> Result<(), CallError>;
    async fn press_key(&self, caller: &Caller, id: u32, key: String) -> Result<(), CallError>;
    async fn destroy(&self, caller: &Caller, id: u32) -> Result<(), CallError>;
    /// Ends the screens `plugin` holds.
    fn release(&self, _plugin: &str) {}
}

/// One system's virtual screens. Calls block.
pub trait ScreenBackend: Send + Sync + 'static {
    /// A running screen, as the backend keeps it.
    type Screen: Send + 'static;

    fn start(&self, width: u32, height: u32) -> Result<(Self::Screen, String), CallError>;
    fn launch(
        &self,
        screen: &mut Self::Screen,
        program: &str,
        args: &[String],
    ) -> Result<u32, CallError>;
    /// The processes still running on it.
    fn alive(&self, screen: &mut Self::Screen) -> Vec<u32>;
    fn capture(&self, screen: &Self::Screen, path: &std::path::Path) -> Result<(), CallError>;
    fn click(&self, screen: &Self::Screen, x: i32, y: i32) -> Result<(), CallError>;
    fn type_text(&self, screen: &Self::Screen, text: &str) -> Result<(), CallError>;
    fn press_key(&self, screen: &Self::Screen, chord: &Chord) -> Result<(), CallError>;
    /// Ends the screen and what runs on it.
    fn stop(&self, screen: Self::Screen);
}

struct Held<S> {
    owner: String,
    name: String,
    width: u32,
    height: u32,
    screen: S,
}

pub struct VirtualScreens<B: ScreenBackend> {
    inner: Arc<Inner<B>>,
}

struct Inner<B: ScreenBackend> {
    backend: B,
    screens: Mutex<BTreeMap<u32, Held<B::Screen>>>,
    next: Mutex<u32>,
    /// Holds `<plugin>/screenshots/` for each caller.
    data_dir: PathBuf,
}

impl<B: ScreenBackend> VirtualScreens<B> {
    pub fn new(backend: B, data_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend,
                screens: Mutex::new(BTreeMap::new()),
                next: Mutex::new(1),
                data_dir,
            }),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Inner<B>) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || f(&inner))
            .await
            .map_err(CallError::failed)?
    }

    /// Runs `f` on caller's screen `id`.
    async fn with<T: Send + 'static>(
        &self,
        caller: &Caller,
        id: u32,
        f: impl FnOnce(&B, &mut B::Screen) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let owner = caller.plugin.clone();
        self.blocking(move |inner| {
            let mut screens = inner.screens.lock().unwrap();
            match screens.get_mut(&id) {
                Some(h) if h.owner == owner => f(&inner.backend, &mut h.screen),
                _ => Err(CallError::Failed(format!("no virtual screen {id}"))),
            }
        })
        .await
    }
}

fn info<S, B: ScreenBackend<Screen = S>>(b: &B, id: u32, h: &mut Held<S>) -> VirtualScreenInfo {
    VirtualScreenInfo {
        id,
        name: h.name.clone(),
        width: h.width,
        height: h.height,
        pids: b.alive(&mut h.screen),
    }
}

#[async_trait]
impl<B: ScreenBackend> VirtualScreenProvider for VirtualScreens<B> {
    async fn create(
        &self,
        caller: &Caller,
        width: u32,
        height: u32,
    ) -> Result<VirtualScreenInfo, CallError> {
        if !(64..=MAX_SIDE).contains(&width) || !(64..=MAX_SIDE).contains(&height) {
            return Err(CallError::Failed(format!(
                "invalid_argument: a virtual screen is 64 to {MAX_SIDE} pixels on each side"
            )));
        }
        let owner = caller.plugin.clone();
        self.blocking(move |inner| {
            let held = inner
                .screens
                .lock()
                .unwrap()
                .values()
                .filter(|h| h.owner == owner)
                .count();
            if held >= MAX_PER_CALLER {
                return Err(CallError::Failed(format!(
                    "{owner} already holds {MAX_PER_CALLER} virtual screens; destroy one first"
                )));
            }
            let (screen, name) = inner.backend.start(width, height)?;
            let id = {
                let mut next = inner.next.lock().unwrap();
                let id = *next;
                *next += 1;
                id
            };
            let mut h = Held {
                owner,
                name,
                width,
                height,
                screen,
            };
            let out = info(&inner.backend, id, &mut h);
            inner.screens.lock().unwrap().insert(id, h);
            Ok(out)
        })
        .await
    }

    async fn list(&self, caller: &Caller) -> Result<Vec<VirtualScreenInfo>, CallError> {
        let owner = caller.plugin.clone();
        self.blocking(move |inner| {
            let mut screens = inner.screens.lock().unwrap();
            Ok(screens
                .iter_mut()
                .filter(|(_, h)| h.owner == owner)
                .map(|(id, h)| info(&inner.backend, *id, h))
                .collect())
        })
        .await
    }

    async fn launch(
        &self,
        caller: &Caller,
        id: u32,
        program: String,
        args: Vec<String>,
    ) -> Result<u32, CallError> {
        caller.shell.check(&program, &[])?;
        self.with(caller, id, move |b, s| b.launch(s, &program, &args))
            .await
    }

    async fn capture(&self, caller: &Caller, id: u32) -> Result<String, CallError> {
        let dir = self.inner.data_dir.join(&caller.plugin).join("screenshots");
        self.with(caller, id, move |b, s| {
            std::fs::create_dir_all(&dir).map_err(CallError::failed)?;
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let path = dir.join(format!("virtual-{id}-{ms}.png"));
            b.capture(s, &path)?;
            Ok(path.display().to_string())
        })
        .await
    }

    async fn click(&self, caller: &Caller, id: u32, x: i32, y: i32) -> Result<(), CallError> {
        self.with(caller, id, move |b, s| b.click(s, x, y)).await
    }

    async fn type_text(&self, caller: &Caller, id: u32, text: String) -> Result<(), CallError> {
        crate::keyboard::check_text(&text)?;
        self.with(caller, id, move |b, s| b.type_text(s, &text))
            .await
    }

    async fn press_key(&self, caller: &Caller, id: u32, key: String) -> Result<(), CallError> {
        let chord = Chord::parse(&key).map_err(CallError::Failed)?;
        self.with(caller, id, move |b, s| b.press_key(s, &chord))
            .await
    }

    async fn destroy(&self, caller: &Caller, id: u32) -> Result<(), CallError> {
        let owner = caller.plugin.clone();
        self.blocking(move |inner| {
            let held = {
                let mut screens = inner.screens.lock().unwrap();
                match screens.get(&id) {
                    Some(h) if h.owner == owner => screens.remove(&id),
                    _ => None,
                }
            };
            match held {
                Some(h) => {
                    inner.backend.stop(h.screen);
                    Ok(())
                }
                None => Err(CallError::Failed(format!("no virtual screen {id}"))),
            }
        })
        .await
    }

    fn release(&self, plugin: &str) {
        let gone: Vec<Held<B::Screen>> = {
            let mut screens = self.inner.screens.lock().unwrap();
            let ids: Vec<u32> = screens
                .iter()
                .filter(|(_, h)| h.owner == plugin)
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| screens.remove(&id))
                .collect()
        };
        for h in gone {
            self.inner.backend.stop(h.screen);
        }
    }
}

impl<B: ScreenBackend> Drop for Inner<B> {
    fn drop(&mut self) {
        let screens = std::mem::take(&mut *self.screens.lock().unwrap());
        for (_, h) in screens {
            self.backend.stop(h.screen);
        }
    }
}

/// No virtual screens on this system yet.
pub struct NoVirtualScreen;

#[async_trait]
impl VirtualScreenProvider for NoVirtualScreen {
    async fn create(&self, _: &Caller, _: u32, _: u32) -> Result<VirtualScreenInfo, CallError> {
        Err(CallError::Unavailable)
    }
    async fn list(&self, _: &Caller) -> Result<Vec<VirtualScreenInfo>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn launch(
        &self,
        _: &Caller,
        _: u32,
        _: String,
        _: Vec<String>,
    ) -> Result<u32, CallError> {
        Err(CallError::Unavailable)
    }
    async fn capture(&self, _: &Caller, _: u32) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
    async fn click(&self, _: &Caller, _: u32, _: i32, _: i32) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn type_text(&self, _: &Caller, _: u32, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn press_key(&self, _: &Caller, _: u32, _: String) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
    async fn destroy(&self, _: &Caller, _: u32) -> Result<(), CallError> {
        Err(CallError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::ShellGrant;

    /// Records what happens to each screen.
    #[derive(Default, Clone)]
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
    }

    struct FakeScreen {
        n: u32,
        pids: Vec<u32>,
    }

    impl ScreenBackend for Fake {
        type Screen = FakeScreen;

        fn start(&self, w: u32, h: u32) -> Result<(FakeScreen, String), CallError> {
            let mut log = self.log.lock().unwrap();
            let n = 99 + log.iter().filter(|l| l.starts_with("start")).count() as u32;
            log.push(format!("start {w}x{h}"));
            Ok((
                FakeScreen {
                    n,
                    pids: Vec::new(),
                },
                format!(":{n}"),
            ))
        }
        fn launch(&self, s: &mut FakeScreen, p: &str, a: &[String]) -> Result<u32, CallError> {
            self.log
                .lock()
                .unwrap()
                .push(format!(":{} {p} {}", s.n, a.join(" ")));
            let pid = 1000 + s.pids.len() as u32;
            s.pids.push(pid);
            Ok(pid)
        }
        fn alive(&self, s: &mut FakeScreen) -> Vec<u32> {
            s.pids.clone()
        }
        fn capture(&self, _: &FakeScreen, path: &std::path::Path) -> Result<(), CallError> {
            std::fs::write(path, b"png").map_err(CallError::failed)
        }
        fn click(&self, s: &FakeScreen, x: i32, y: i32) -> Result<(), CallError> {
            self.log
                .lock()
                .unwrap()
                .push(format!(":{} click {x},{y}", s.n));
            Ok(())
        }
        fn type_text(&self, s: &FakeScreen, t: &str) -> Result<(), CallError> {
            self.log.lock().unwrap().push(format!(":{} type {t}", s.n));
            Ok(())
        }
        fn press_key(&self, s: &FakeScreen, c: &Chord) -> Result<(), CallError> {
            self.log.lock().unwrap().push(format!(":{} key {c}", s.n));
            Ok(())
        }
        fn stop(&self, s: FakeScreen) {
            self.log.lock().unwrap().push(format!("stop :{}", s.n));
        }
    }

    fn caller(name: &str, programs: &[&str]) -> Caller {
        Caller {
            plugin: name.into(),
            shell: ShellGrant {
                programs: programs.iter().map(|p| p.to_string()).collect(),
                env: Vec::new(),
            },
            ..Caller::default()
        }
    }

    #[tokio::test]
    async fn creates_drives_and_destroys_a_screen() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let v = VirtualScreens::new(fake.clone(), dir.path().to_path_buf());
        let a = caller("a", &["/usr/bin/xterm"]);
        let s = v.create(&a, 1280, 800).await.unwrap();
        assert_eq!((s.id, s.name.as_str(), s.width), (1, ":99", 1280));
        let pid = v
            .launch(
                &a,
                1,
                "/usr/bin/xterm".into(),
                vec!["-e".into(), "top".into()],
            )
            .await
            .unwrap();
        assert_eq!(v.list(&a).await.unwrap()[0].pids, [pid]);
        v.click(&a, 1, 10, 20).await.unwrap();
        v.type_text(&a, 1, "hi".into()).await.unwrap();
        v.press_key(&a, 1, "ctrl+s".into()).await.unwrap();
        let png = v.capture(&a, 1).await.unwrap();
        assert!(png.starts_with(&dir.path().join("a/screenshots").display().to_string()));
        v.destroy(&a, 1).await.unwrap();
        assert!(v.list(&a).await.unwrap().is_empty());
        assert_eq!(
            *fake.log.lock().unwrap(),
            [
                "start 1280x800",
                ":99 /usr/bin/xterm -e top",
                ":99 click 10,20",
                ":99 type hi",
                ":99 key ctrl+KeyS",
                "stop :99"
            ]
        );
    }

    #[tokio::test]
    async fn launch_needs_the_shell_grant_and_screens_stay_with_their_owner() {
        let dir = tempfile::tempdir().unwrap();
        let v = VirtualScreens::new(Fake::default(), dir.path().to_path_buf());
        let a = caller("a", &["/usr/bin/xterm"]);
        let b = caller("b", &[]);
        let s = v.create(&a, 800, 600).await.unwrap();
        assert_eq!(
            v.launch(&a, s.id, "/bin/rm".into(), vec![]).await,
            Err(CallError::Denied)
        );
        assert_eq!(
            v.launch(&a, s.id, "xterm".into(), vec![]).await,
            Err(CallError::Denied)
        );
        assert!(v.click(&b, s.id, 1, 1).await.is_err());
        assert!(v.destroy(&b, s.id).await.is_err());
        assert!(v.list(&b).await.unwrap().is_empty());
        assert!(v.create(&a, 10, 600).await.is_err());
        for _ in 1..MAX_PER_CALLER {
            v.create(&a, 800, 600).await.unwrap();
        }
        assert!(v.create(&a, 800, 600).await.is_err());
        assert!(v.press_key(&a, s.id, "hyper+x".into()).await.is_err());
    }

    #[tokio::test]
    async fn release_ends_a_plugins_screens() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let v = VirtualScreens::new(fake.clone(), dir.path().to_path_buf());
        v.create(&caller("a", &[]), 800, 600).await.unwrap();
        v.create(&caller("b", &[]), 800, 600).await.unwrap();
        v.release("a");
        assert!(fake.log.lock().unwrap().contains(&"stop :99".to_string()));
        assert_eq!(v.list(&caller("b", &[])).await.unwrap().len(), 1);
        drop(v);
        assert!(fake.log.lock().unwrap().contains(&"stop :100".to_string()));
    }
}
