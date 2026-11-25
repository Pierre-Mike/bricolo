//! Running a system program with a deadline, behind a [`Runner`] so the
//! command plans of `device`, `scripting` and `virtual-screen` are tested
//! with a fake one on any system.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::CallError;

/// What a program left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    /// Stdout trimmed, or `failed` with stderr when the exit code says so.
    pub fn text(self, what: &str) -> Result<String, CallError> {
        if self.ok() {
            Ok(self.stdout.trim().to_string())
        } else {
            let err = if self.stderr.trim().is_empty() {
                self.stdout.trim()
            } else {
                self.stderr.trim()
            };
            Err(CallError::Failed(format!("{what} failed: {err}")))
        }
    }
}

/// Why a program did not finish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunError {
    /// The program is not installed.
    Missing,
    Timeout,
    Other(String),
}

impl From<RunError> for CallError {
    fn from(e: RunError) -> Self {
        match e {
            RunError::Missing => CallError::Unavailable,
            RunError::Timeout => CallError::Timeout,
            RunError::Other(m) => CallError::Failed(m),
        }
    }
}

pub trait Runner: Send + Sync + 'static {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        stdin: Option<&str>,
        timeout: Duration,
    ) -> Result<Ran, RunError>;
}

/// Runs real programs.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        stdin: Option<&str>,
        timeout: Duration,
    ) -> Result<Ran, RunError> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flashes up.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                RunError::Missing
            } else {
                RunError::Other(format!("cannot start {program}: {e}"))
            }
        })?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let text = text.to_string();
            std::thread::spawn(move || {
                let _ = pipe.write_all(text.as_bytes());
            });
        }
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut s = String::new();
                if let Some(mut p) = pipe {
                    let mut buf = Vec::new();
                    let _ = p.read_to_end(&mut buf);
                    s = String::from_utf8_lossy(&buf).into_owned();
                }
                s
            })
        };
        let out = drain(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let err = drain(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Timeout);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(15)),
                Err(e) => return Err(RunError::Other(e.to_string())),
            }
        };
        Ok(Ran {
            code: status.code(),
            stdout: out.join().unwrap_or_default(),
            stderr: err.join().unwrap_or_default(),
        })
    }
}

/// A runner that answers from a script of expected calls, and records
/// them. For tests.
/// Answers by the start of the call text.
type Answers = Arc<Mutex<Vec<(String, Result<Ran, RunError>)>>>;

#[derive(Clone, Default)]
pub struct FakeRunner {
    /// Each call as `program arg arg…`, with stdin after ` <<< `.
    pub calls: Arc<Mutex<Vec<String>>>,
    /// Answers by the start of the call text; the first match wins.
    pub answers: Answers,
}

impl FakeRunner {
    /// Answers calls starting with `prefix` with `stdout` and exit 0.
    pub fn answer(&self, prefix: &str, stdout: &str) -> &Self {
        self.answers.lock().unwrap().push((
            prefix.to_string(),
            Ok(Ran {
                code: Some(0),
                stdout: stdout.to_string(),
                stderr: String::new(),
            }),
        ));
        self
    }

    pub fn fail(&self, prefix: &str, code: i32, stderr: &str) -> &Self {
        self.answers.lock().unwrap().push((
            prefix.to_string(),
            Ok(Ran {
                code: Some(code),
                stdout: String::new(),
                stderr: stderr.to_string(),
            }),
        ));
        self
    }

    pub fn error(&self, prefix: &str, e: RunError) -> &Self {
        self.answers
            .lock()
            .unwrap()
            .push((prefix.to_string(), Err(e)));
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Runner for FakeRunner {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        stdin: Option<&str>,
        _: Duration,
    ) -> Result<Ran, RunError> {
        let mut call = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(s) = stdin {
            call.push_str(" <<< ");
            call.push_str(s);
        }
        self.calls.lock().unwrap().push(call.clone());
        let answers = self.answers.lock().unwrap();
        answers
            .iter()
            .find(|(p, _)| call.starts_with(p.as_str()))
            .map(|(_, a)| a.clone())
            .unwrap_or_else(|| {
                Ok(Ran {
                    code: Some(0),
                    ..Ran::default()
                })
            })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn runs_with_stdin_and_a_deadline() {
        let r = SystemRunner
            .run("/bin/cat", &[], Some("hello"), Duration::from_secs(5))
            .unwrap();
        assert_eq!(r.stdout, "hello");
        assert!(r.ok());
        let e = SystemRunner
            .run("/bin/sleep", &["5"], None, Duration::from_millis(100))
            .unwrap_err();
        assert_eq!(e, RunError::Timeout);
        assert_eq!(
            SystemRunner
                .run("/no/such/program", &[], None, Duration::from_secs(1))
                .unwrap_err(),
            RunError::Missing
        );
        let r = SystemRunner
            .run(
                "/bin/sh",
                &["-c", "echo oops >&2; exit 3"],
                None,
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            r.text("sh"),
            Err(CallError::Failed("sh failed: oops".into()))
        );
    }
}
