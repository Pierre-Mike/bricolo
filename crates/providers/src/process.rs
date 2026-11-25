//! `process`: the programs running on this device, and stopping them. The
//! guards (which processes the node refuses to touch, the order of the
//! list) live in [`Processes`] over a [`ProcessBackend`]; the backend reads
//! through the `sysinfo` crate on every system ([`SysinfoBackend`]). Tests
//! use a fake backend.
//!
//! `process` never runs on a peer through rules.toml; the MCP gateway
//! reaches it on each node by name, and marks `stop` and `kill` as
//! destructive so agents ask first.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::caller::Caller;
use crate::error::CallError;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub command: Vec<String>,
    pub user: Option<String>,
    pub cpu: f32,
    pub memory: u64,
    pub started: u64,
}

#[async_trait]
pub trait ProcessProvider: Send + Sync {
    async fn list(&self, caller: &Caller) -> Result<Vec<ProcessInfo>, CallError>;
    async fn get(&self, caller: &Caller, pid: u32) -> Result<Option<ProcessInfo>, CallError>;
    async fn stop(&self, caller: &Caller, pid: u32) -> Result<(), CallError>;
    async fn kill(&self, caller: &Caller, pid: u32) -> Result<(), CallError>;
}

/// How a process is asked to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// SIGTERM; on Windows, close its windows.
    Stop,
    /// SIGKILL; TerminateProcess.
    Kill,
}

/// One system's process table. Calls block.
pub trait ProcessBackend: Send + Sync + 'static {
    fn list(&self) -> Result<Vec<ProcessInfo>, CallError>;
    fn end(&self, pid: u32, how: Ending) -> Result<(), CallError>;
}

/// The guards over a backend.
pub struct Processes<B: ProcessBackend> {
    backend: Arc<B>,
    /// Never stopped: the node, its parent (the hub or the service
    /// manager), and the system's first process.
    protected: Vec<u32>,
}

impl<B: ProcessBackend> Processes<B> {
    pub fn new(backend: B) -> Self {
        #[cfg(unix)]
        let protected = vec![
            0,
            1,
            std::process::id(),
            std::os::unix::process::parent_id(),
        ];
        #[cfg(not(unix))]
        let protected = vec![0, 1, std::process::id()];
        Self::with_protected(backend, protected)
    }

    pub fn with_protected(backend: B, protected: Vec<u32>) -> Self {
        Self {
            backend: Arc::new(backend),
            protected,
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&B) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let b = self.backend.clone();
        tokio::task::spawn_blocking(move || f(&b))
            .await
            .map_err(CallError::failed)?
    }

    async fn end(&self, pid: u32, how: Ending) -> Result<(), CallError> {
        if self.protected.contains(&pid) {
            return Err(CallError::Failed(format!(
                "process {pid} is the node, its parent or the system's first process; the node \
                 does not stop it"
            )));
        }
        self.blocking(move |b| {
            if !b.list()?.iter().any(|p| p.pid == pid) {
                return Err(CallError::Failed(format!("no process {pid}")));
            }
            b.end(pid, how)
        })
        .await
    }
}

#[async_trait]
impl<B: ProcessBackend> ProcessProvider for Processes<B> {
    async fn list(&self, _: &Caller) -> Result<Vec<ProcessInfo>, CallError> {
        self.blocking(|b| {
            let mut list = b.list()?;
            list.sort_by(|a, b| {
                b.cpu
                    .total_cmp(&a.cpu)
                    .then(b.memory.cmp(&a.memory))
                    .then(a.pid.cmp(&b.pid))
            });
            Ok(list)
        })
        .await
    }

    async fn get(&self, _: &Caller, pid: u32) -> Result<Option<ProcessInfo>, CallError> {
        self.blocking(move |b| Ok(b.list()?.into_iter().find(|p| p.pid == pid)))
            .await
    }

    async fn stop(&self, _: &Caller, pid: u32) -> Result<(), CallError> {
        self.end(pid, Ending::Stop).await
    }

    async fn kill(&self, _: &Caller, pid: u32) -> Result<(), CallError> {
        self.end(pid, Ending::Kill).await
    }
}

/// The process table through `sysinfo`. CPU use needs two samples; the
/// first list after a quiet second waits 250 ms for the second.
#[derive(Default)]
pub struct SysinfoBackend {
    /// Made on the first call.
    state: Mutex<Option<Table>>,
}

struct Table {
    system: sysinfo::System,
    users: sysinfo::Users,
    last: Option<Instant>,
}

impl SysinfoBackend {
    fn with<T>(&self, f: impl FnOnce(&mut Table) -> Result<T, CallError>) -> Result<T, CallError> {
        let mut guard = self.state.lock().unwrap();
        if guard.is_none() {
            *guard = Some(Table {
                system: sysinfo::System::new().map_err(CallError::failed)?,
                users: sysinfo::Users::new_with_refreshed_list().map_err(CallError::failed)?,
                last: None,
            });
        }
        f(guard.as_mut().expect("made above"))
    }
}

impl SysinfoBackend {
    fn refresh(system: &mut sysinfo::System) {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_cmd(UpdateKind::OnlyIfNotSet),
        );
    }
}

impl ProcessBackend for SysinfoBackend {
    fn list(&self) -> Result<Vec<ProcessInfo>, CallError> {
        self.with(|t| {
            let Table {
                system,
                users,
                last,
            } = t;
            let stale = last.is_none_or(|t| t.elapsed() > Duration::from_secs(1));
            Self::refresh(system);
            if stale {
                std::thread::sleep(
                    sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.max(Duration::from_millis(250)),
                );
                Self::refresh(system);
            }
            *last = Some(Instant::now());
            Ok(system
                .processes()
                .values()
                // Linux lists each thread as a task of its process.
                .filter(|p| p.thread_kind().is_none())
                .map(|p| ProcessInfo {
                    pid: p.pid().as_u32(),
                    parent: p.parent().map(|p| p.as_u32()),
                    name: p.name().to_string_lossy().into_owned(),
                    command: p
                        .cmd()
                        .iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect(),
                    user: p
                        .user_id()
                        .and_then(|u| users.get_user_by_id(u))
                        .map(|u| u.name().to_string()),
                    cpu: p.cpu_usage(),
                    memory: p.memory(),
                    started: p.start_time(),
                })
                .collect())
        })
    }

    fn end(&self, pid: u32, how: Ending) -> Result<(), CallError> {
        #[cfg(windows)]
        if how == Ending::Stop {
            // taskkill without /F sends WM_CLOSE to the process's windows.
            let out = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string()])
                .output()
                .map_err(CallError::failed)?;
            return if out.status.success() {
                Ok(())
            } else {
                Err(CallError::Failed(
                    String::from_utf8_lossy(&out.stderr).trim().to_string(),
                ))
            };
        }
        self.with(|t| {
            let p = t
                .system
                .process(sysinfo::Pid::from_u32(pid))
                .ok_or_else(|| CallError::Failed(format!("no process {pid}")))?;
            let signal = match how {
                Ending::Stop => sysinfo::Signal::Term,
                Ending::Kill => sysinfo::Signal::Kill,
            };
            match p.kill_with(signal) {
                Some(true) => Ok(()),
                Some(false) => Err(CallError::Failed(format!(
                    "the system refused to signal process {pid}; it may belong to another user"
                ))),
                None => Err(CallError::Unavailable),
            }
        })
    }
}

/// The process table of this device.
pub fn local() -> Arc<dyn ProcessProvider> {
    Arc::new(Processes::new(SysinfoBackend::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        ended: Mutex<Vec<(u32, Ending)>>,
    }

    fn info(pid: u32, name: &str, cpu: f32, memory: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent: Some(1),
            name: name.into(),
            command: vec![format!("/bin/{name}")],
            user: Some("me".into()),
            cpu,
            memory,
            started: 0,
        }
    }

    impl ProcessBackend for Fake {
        fn list(&self) -> Result<Vec<ProcessInfo>, CallError> {
            let ended = self.ended.lock().unwrap();
            Ok([
                info(1, "launchd", 0.1, 10),
                info(10, "idle", 0.0, 5),
                info(20, "busy", 90.0, 100),
                info(30, "fat", 0.0, 9000),
            ]
            .into_iter()
            .filter(|p| !ended.iter().any(|(e, _)| *e == p.pid))
            .collect())
        }

        fn end(&self, pid: u32, how: Ending) -> Result<(), CallError> {
            self.ended.lock().unwrap().push((pid, how));
            Ok(())
        }
    }

    fn procs() -> Processes<Fake> {
        Processes::with_protected(Fake::default(), vec![0, 1, 99])
    }

    #[tokio::test]
    async fn lists_the_busiest_first_and_gets_one() {
        let p = procs();
        let c = Caller::new("t");
        let names: Vec<String> = p
            .list(&c)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["busy", "launchd", "fat", "idle"]);
        assert_eq!(p.get(&c, 30).await.unwrap().unwrap().memory, 9000);
        assert_eq!(p.get(&c, 31).await.unwrap(), None);
    }

    #[tokio::test]
    async fn stops_and_kills_but_never_the_protected_ones() {
        let p = procs();
        let c = Caller::new("t");
        p.stop(&c, 20).await.unwrap();
        p.kill(&c, 30).await.unwrap();
        assert_eq!(
            *p.backend.ended.lock().unwrap(),
            [(20, Ending::Stop), (30, Ending::Kill)]
        );
        for pid in [0, 1, 99] {
            let e = p.kill(&c, pid).await.unwrap_err();
            assert!(matches!(e, CallError::Failed(m) if m.contains("does not stop it")));
        }
        let e = p.stop(&c, 20).await.unwrap_err();
        assert_eq!(e, CallError::Failed("no process 20".into()));
    }

    #[tokio::test]
    async fn the_real_table_holds_this_process() {
        let p = Processes::new(SysinfoBackend::default());
        let c = Caller::new("t");
        let me = p.get(&c, std::process::id()).await.unwrap().unwrap();
        assert!(me.memory > 0);
        assert!(!me.name.is_empty());
        // The node refuses to stop itself.
        assert!(p.kill(&c, std::process::id()).await.is_err());
    }
}
