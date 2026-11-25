//! `scripting`: AppleScript, JavaScript for Automation and Shortcuts on
//! macOS (`osascript`, `shortcuts`), and PowerShell (`powershell.exe` on
//! Windows, `pwsh` wherever it is installed). Every script goes through a
//! [`Runner`] with a one-minute deadline; the tests use a fake one.
//!
//! `scripting` never runs on a peer through rules.toml. The MCP gateway
//! marks the `run-*` methods destructive, so agents ask before running one.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::caller::Caller;
use crate::cmd::{RunError, Runner};
use crate::device::Os;
use crate::error::CallError;

pub const TIMEOUT: Duration = Duration::from_secs(60);
/// Scripts larger than this are refused.
pub const MAX_SOURCE: usize = 256 * 1024;

#[async_trait]
pub trait ScriptingProvider: Send + Sync {
    async fn run_applescript(&self, caller: &Caller, source: String) -> Result<String, CallError>;
    async fn run_jxa(&self, caller: &Caller, source: String) -> Result<String, CallError>;
    async fn list_shortcuts(&self, caller: &Caller) -> Result<Vec<String>, CallError>;
    async fn run_shortcut(
        &self,
        caller: &Caller,
        name: String,
        input: Option<String>,
    ) -> Result<String, CallError>;
    async fn run_powershell(&self, caller: &Caller, source: String) -> Result<String, CallError>;
}

pub struct Scripting<R: Runner> {
    inner: Arc<(Os, R, PathBuf)>,
}

impl<R: Runner> Scripting<R> {
    /// `temp` holds the input and output files of shortcuts and the
    /// PowerShell scripts while they run.
    pub fn new(os: Os, runner: R, temp: PathBuf) -> Self {
        Self {
            inner: Arc::new((os, runner, temp)),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(Os, &R, &PathBuf) -> Result<T, CallError> + Send + 'static,
    ) -> Result<T, CallError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || f(inner.0, &inner.1, &inner.2))
            .await
            .map_err(CallError::failed)?
    }
}

fn check(source: &str) -> Result<(), CallError> {
    if source.len() > MAX_SOURCE {
        return Err(CallError::Failed(format!(
            "the script has {} bytes; at most {MAX_SOURCE}",
            source.len()
        )));
    }
    Ok(())
}

fn only_macos(os: Os) -> Result<(), CallError> {
    if os == Os::MacOs {
        Ok(())
    } else {
        Err(CallError::Unavailable)
    }
}

fn run<R: Runner>(
    r: &R,
    program: &str,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<String, CallError> {
    let out = r.run(program, args, stdin, TIMEOUT).map_err(|e| match e {
        RunError::Missing => CallError::Unavailable,
        e => e.into(),
    })?;
    // Scripts print their own trailing newline; keep inner whitespace.
    if out.ok() {
        Ok(out.stdout.trim_end_matches(['\n', '\r']).to_string())
    } else {
        Err(CallError::Failed(out.stderr.trim().to_string()))
    }
}

/// A file name that no other run uses.
fn unique(temp: &std::path::Path, ext: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    temp.join(format!(
        "bricolo-{}-{}.{ext}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

#[async_trait]
impl<R: Runner> ScriptingProvider for Scripting<R> {
    async fn run_applescript(&self, _: &Caller, source: String) -> Result<String, CallError> {
        check(&source)?;
        self.blocking(move |os, r, _| {
            only_macos(os)?;
            run(
                r,
                "/usr/bin/osascript",
                &["-l", "AppleScript", "-"],
                Some(&source),
            )
        })
        .await
    }

    async fn run_jxa(&self, _: &Caller, source: String) -> Result<String, CallError> {
        check(&source)?;
        self.blocking(move |os, r, _| {
            only_macos(os)?;
            run(
                r,
                "/usr/bin/osascript",
                &["-l", "JavaScript", "-"],
                Some(&source),
            )
        })
        .await
    }

    async fn list_shortcuts(&self, _: &Caller) -> Result<Vec<String>, CallError> {
        self.blocking(|os, r, _| {
            only_macos(os)?;
            let out = run(r, "/usr/bin/shortcuts", &["list"], None)?;
            Ok(out
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect())
        })
        .await
    }

    async fn run_shortcut(
        &self,
        _: &Caller,
        name: String,
        input: Option<String>,
    ) -> Result<String, CallError> {
        self.blocking(move |os, r, temp| {
            only_macos(os)?;
            std::fs::create_dir_all(temp).map_err(CallError::failed)?;
            let output = unique(temp, "txt");
            let input_file = match &input {
                Some(text) => {
                    let f = unique(temp, "txt");
                    std::fs::write(&f, text).map_err(CallError::failed)?;
                    Some(f)
                }
                None => None,
            };
            let out_s = output.display().to_string();
            let in_s = input_file.as_ref().map(|f| f.display().to_string());
            let mut args = vec![
                "run",
                name.as_str(),
                "--output-path",
                &out_s,
                "--output-type",
                "public.plain-text",
            ];
            if let Some(i) = &in_s {
                args.push("--input-path");
                args.push(i);
            }
            let result = run(r, "/usr/bin/shortcuts", &args, None);
            if let Some(f) = &input_file {
                let _ = std::fs::remove_file(f);
            }
            let printed = result?;
            let text = std::fs::read_to_string(&output).unwrap_or(printed);
            let _ = std::fs::remove_file(&output);
            Ok(text.trim_end_matches(['\n', '\r']).to_string())
        })
        .await
    }

    async fn run_powershell(&self, _: &Caller, source: String) -> Result<String, CallError> {
        check(&source)?;
        self.blocking(move |os, r, temp| {
            std::fs::create_dir_all(temp).map_err(CallError::failed)?;
            let file = unique(temp, "ps1");
            std::fs::write(&file, &source).map_err(CallError::failed)?;
            let path = file.display().to_string();
            let (program, args): (&str, Vec<&str>) = match os {
                Os::Windows => (
                    "powershell.exe",
                    vec![
                        "-NoProfile",
                        "-NonInteractive",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        &path,
                    ],
                ),
                _ => (
                    "pwsh",
                    vec!["-NoProfile", "-NonInteractive", "-File", &path],
                ),
            };
            let result = run(r, program, &args, None);
            let _ = std::fs::remove_file(&file);
            result
        })
        .await
    }
}

/// The scripting of this device.
pub fn local(temp: PathBuf) -> Arc<dyn ScriptingProvider> {
    match Os::here() {
        Some(os) => Arc::new(Scripting::new(os, crate::cmd::SystemRunner, temp)),
        None => Arc::new(NoScripting),
    }
}

/// No scripting on this node.
pub struct NoScripting;

#[async_trait]
impl ScriptingProvider for NoScripting {
    async fn run_applescript(&self, _: &Caller, _: String) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
    async fn run_jxa(&self, _: &Caller, _: String) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
    async fn list_shortcuts(&self, _: &Caller) -> Result<Vec<String>, CallError> {
        Err(CallError::Unavailable)
    }
    async fn run_shortcut(
        &self,
        _: &Caller,
        _: String,
        _: Option<String>,
    ) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
    async fn run_powershell(&self, _: &Caller, _: String) -> Result<String, CallError> {
        Err(CallError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::FakeRunner;

    fn c() -> Caller {
        Caller::new("t")
    }

    #[tokio::test]
    async fn applescript_and_jxa_go_through_osascript_stdin() {
        let r = FakeRunner::default();
        r.answer("/usr/bin/osascript -l AppleScript", "42\n").fail(
            "/usr/bin/osascript -l JavaScript",
            1,
            "execution error: boom (-2700)",
        );
        let s = Scripting::new(Os::MacOs, r.clone(), std::env::temp_dir());
        assert_eq!(
            s.run_applescript(&c(), "return 6 * 7".into())
                .await
                .unwrap(),
            "42"
        );
        let e = s.run_jxa(&c(), "throw 'boom'".into()).await.unwrap_err();
        assert_eq!(e, CallError::Failed("execution error: boom (-2700)".into()));
        assert_eq!(
            r.calls(),
            [
                "/usr/bin/osascript -l AppleScript - <<< return 6 * 7",
                "/usr/bin/osascript -l JavaScript - <<< throw 'boom'"
            ]
        );
        let big = "x".repeat(MAX_SOURCE + 1);
        assert!(s.run_applescript(&c(), big).await.is_err());
    }

    #[tokio::test]
    async fn shortcuts_list_and_run_with_input() {
        let dir = tempfile::tempdir().unwrap();
        let r = FakeRunner::default();
        r.answer("/usr/bin/shortcuts list", "Morning\n\nResize Image\n");
        let s = Scripting::new(Os::MacOs, r.clone(), dir.path().to_path_buf());
        assert_eq!(
            s.list_shortcuts(&c()).await.unwrap(),
            ["Morning", "Resize Image"]
        );
        s.run_shortcut(&c(), "Morning".into(), Some("hi".into()))
            .await
            .unwrap();
        let call = r.calls().pop().unwrap();
        assert!(
            call.starts_with("/usr/bin/shortcuts run Morning --output-path "),
            "{call}"
        );
        assert!(call.contains("--output-type public.plain-text --input-path "));
        // The input file is gone once the shortcut ran.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn powershell_runs_a_file_and_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let r = FakeRunner::default();
        r.answer("powershell.exe", "hello\r\n")
            .error("pwsh", RunError::Timeout);
        let win = Scripting::new(Os::Windows, r.clone(), dir.path().to_path_buf());
        assert_eq!(
            win.run_powershell(&c(), "'hello'".into()).await.unwrap(),
            "hello"
        );
        assert!(r.calls()[0].starts_with(
            "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "
        ));
        let mac = Scripting::new(Os::MacOs, r.clone(), dir.path().to_path_buf());
        assert_eq!(
            mac.run_powershell(&c(), "1".into()).await,
            Err(CallError::Timeout)
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn apple_scripting_is_macos_only_and_missing_tools_are_unavailable() {
        let r = FakeRunner::default();
        r.error("pwsh", RunError::Missing);
        let s = Scripting::new(Os::Linux, r, std::env::temp_dir());
        assert_eq!(
            s.run_applescript(&c(), "1".into()).await,
            Err(CallError::Unavailable)
        );
        assert_eq!(s.list_shortcuts(&c()).await, Err(CallError::Unavailable));
        assert_eq!(
            s.run_powershell(&c(), "1".into()).await,
            Err(CallError::Unavailable)
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn real_osascript_returns_its_value() {
        let s = local(std::env::temp_dir());
        assert_eq!(
            s.run_applescript(&c(), "return 6 * 7".into())
                .await
                .unwrap(),
            "42"
        );
        assert_eq!(
            s.run_jxa(&c(), "[1, 2].map(x => x * 2).join(',')".into())
                .await
                .unwrap(),
            "2,4"
        );
    }
}
