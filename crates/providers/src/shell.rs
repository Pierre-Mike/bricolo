//! `shell`: runs a program from the plugin's `[shell] allow` list and
//! returns its exit code and output. No shell is involved: the program is
//! started directly with the given arguments. The child gets a clean
//! environment: `HOME`, a fixed `PATH`, and the variables the plugin passes,
//! each of which must be listed under `[shell] env`. On Windows, where
//! programs need more of the system's variables to start, the node passes
//! the ones in [`HOST_ENV`] and a `PATH` of the system folders.

use std::path::{Component, Path};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::caller::Caller;
use crate::error::CallError;

/// Bytes kept from each of stdout and stderr; the rest is read and dropped.
pub const OUTPUT_CAP: usize = 1 << 20;
/// How long a program may run before it is killed and the call times out.
pub const TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(not(windows))]
const PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Variables the node sets for every child. A plugin may not override them.
#[cfg(not(windows))]
pub const HOST_ENV: &[&str] = &["HOME", "PATH"];
/// Variables the node sets for every child. A plugin may not override them.
#[cfg(windows)]
pub const HOST_ENV: &[&str] = &[
    "PATH",
    "SystemRoot",
    "windir",
    "ComSpec",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
];

/// Whether the node sets `name` for every child. Windows variable names
/// ignore case.
pub fn is_host_env(name: &str) -> bool {
    HOST_ENV.iter().any(|h| {
        if cfg!(windows) {
            h.eq_ignore_ascii_case(name)
        } else {
            *h == name
        }
    })
}

/// The `PATH` a child gets on Windows: the system folders under
/// `system_root` (`C:\Windows`), as a stock install has them.
pub fn windows_path(system_root: &str) -> String {
    let root = system_root.trim_end_matches('\\');
    [
        format!(r"{root}\System32"),
        root.to_string(),
        format!(r"{root}\System32\Wbem"),
        format!(r"{root}\System32\WindowsPowerShell\v1.0"),
    ]
    .join(";")
}

/// `[shell]` in `plugin.toml`: exact absolute program paths, and the names of
/// the environment variables the plugin may set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellGrant {
    pub programs: Vec<String>,
    pub env: Vec<String>,
}

/// An allow entry: an absolute path without `..`.
pub fn valid_program(path: &str) -> bool {
    let p = Path::new(path);
    p.is_absolute() && !p.components().any(|c| c == Component::ParentDir)
}

/// A variable name: letters, digits and underscores, not starting with a
/// digit.
pub fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The entry, in `programs` and in `env`, that stands for every program or
/// every variable. Only trusted callers (Lua scripts) get it: `plugin.toml`
/// cannot express it, since its entries must be absolute paths and names.
pub const ANY: &str = "*";

impl ShellGrant {
    /// Every absolute program and every variable but `HOME` and `PATH`.
    pub fn any() -> Self {
        Self {
            programs: vec![ANY.into()],
            env: vec![ANY.into()],
        }
    }

    /// `Denied` unless `program` is listed exactly and every name in `env`
    /// is listed and not one the node sets.
    pub fn check(&self, program: &str, env: &[(String, String)]) -> Result<(), CallError> {
        let listed = |list: &[String], item: &str| list.iter().any(|p| p == item || p == ANY);
        if !valid_program(program) || !listed(&self.programs, program) {
            return Err(CallError::Denied);
        }
        for (name, _) in env {
            if is_host_env(name) || !valid_env_name(name) || !listed(&self.env, name) {
                return Err(CallError::Denied);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// `None` when a signal ended the program.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait ShellProvider: Send + Sync {
    async fn run(
        &self,
        caller: &Caller,
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<Output, CallError>;
}

/// Runs programs on this node.
pub struct LocalShell {
    pub timeout: Duration,
}

impl Default for LocalShell {
    fn default() -> Self {
        Self { timeout: TIMEOUT }
    }
}

#[async_trait]
impl ShellProvider for LocalShell {
    async fn run(
        &self,
        caller: &Caller,
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Result<Output, CallError> {
        caller.shell.check(&program, &env)?;
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&args)
            .env_clear()
            .envs(env)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(not(windows))]
        {
            cmd.env("PATH", PATH);
            if let Some(home) = std::env::var_os("HOME") {
                cmd.env("HOME", home);
            }
        }
        #[cfg(windows)]
        {
            for name in HOST_ENV.iter().filter(|n| **n != "PATH") {
                if let Some(value) = std::env::var_os(name) {
                    cmd.env(name, value);
                }
            }
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            cmd.env("PATH", windows_path(&root));
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| CallError::Failed(format!("cannot start {program}: {e}")))?;
        let stdout = tokio::spawn(read_capped(child.stdout.take()));
        let stderr = tokio::spawn(read_capped(child.stderr.take()));
        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(status) => status.map_err(CallError::failed)?,
            Err(_) => {
                let _ = child.kill().await;
                tracing::warn!(plugin = %caller.plugin, "{program} ran past {} s, killed", self.timeout.as_secs());
                return Err(CallError::Timeout);
            }
        };
        Ok(Output {
            code: status.code(),
            stdout: stdout.await.unwrap_or_default(),
            stderr: stderr.await.unwrap_or_default(),
        })
    }
}

/// Reads everything, keeping the first [`OUTPUT_CAP`] bytes as text.
async fn read_capped(pipe: Option<impl AsyncRead + Unpin>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        let room = OUTPUT_CAP.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    String::from_utf8_lossy(&kept).into_owned()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn caller(programs: &[&str], env: &[&str]) -> Caller {
        Caller {
            shell: ShellGrant {
                programs: programs.iter().map(|s| s.to_string()).collect(),
                env: env.iter().map(|s| s.to_string()).collect(),
            },
            ..Caller::new("p")
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn windows_children_get_the_system_folders() {
        let want = r"C:\Windows\System32;C:\Windows;C:\Windows\System32\Wbem;C:\Windows\System32\WindowsPowerShell\v1.0";
        assert_eq!(windows_path(r"C:\Windows"), want);
        assert_eq!(windows_path(r"C:\Windows\"), want);
        assert!(is_host_env("PATH"));
        assert!(!is_host_env("GREETING"));
    }

    #[test]
    fn grant_needs_exact_paths_and_listed_variables() {
        let g = caller(&["/bin/echo"], &["GREETING"]).shell;
        assert!(g.check("/bin/echo", &[]).is_ok());
        assert!(
            g.check("/bin/echo", &[("GREETING".into(), "x".into())])
                .is_ok()
        );
        assert_eq!(g.check("echo", &[]), Err(CallError::Denied));
        assert_eq!(g.check("/bin/ls", &[]), Err(CallError::Denied));
        assert_eq!(g.check("/bin/../bin/echo", &[]), Err(CallError::Denied));
        assert_eq!(
            g.check("/bin/echo", &[("OTHER".into(), "x".into())]),
            Err(CallError::Denied)
        );
        let home = ShellGrant {
            programs: strings(&["/bin/echo"]),
            env: strings(&["HOME"]),
        };
        assert_eq!(
            home.check("/bin/echo", &[("HOME".into(), "/tmp".into())]),
            Err(CallError::Denied)
        );
        // The trusted grant still needs an absolute program and keeps HOME
        // and PATH for the node.
        let any = ShellGrant::any();
        assert!(
            any.check("/bin/ls", &[("OTHER".into(), "x".into())])
                .is_ok()
        );
        assert_eq!(any.check("ls", &[]), Err(CallError::Denied));
        assert_eq!(
            any.check("/bin/ls", &[("PATH".into(), "/".into())]),
            Err(CallError::Denied)
        );
        assert!(!valid_program(ANY));
        assert!(!valid_env_name(ANY));
        assert!(valid_env_name("MY_VAR2"));
        assert!(!valid_env_name("2X"));
        assert!(!valid_env_name("A-B"));
        assert!(valid_program("/usr/bin/say"));
        assert!(!valid_program("bin/say"));
        assert!(!valid_program("/usr/../bin/sh"));
    }

    #[tokio::test]
    async fn runs_an_allowed_program_and_refuses_others() {
        let c = caller(&["/bin/echo"], &[]);
        let out = LocalShell::default()
            .run(&c, "/bin/echo".into(), strings(&["hi", "there"]), vec![])
            .await
            .unwrap();
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout, "hi there\n");
        let denied = LocalShell::default()
            .run(&c, "/bin/ls".into(), vec![], vec![])
            .await;
        assert_eq!(denied, Err(CallError::Denied));
    }

    #[tokio::test]
    async fn child_sees_only_home_path_and_listed_variables() {
        let c = caller(&["/usr/bin/env"], &["GREETING"]);
        let out = LocalShell::default()
            .run(
                &c,
                "/usr/bin/env".into(),
                vec![],
                vec![("GREETING".into(), "hello".into())],
            )
            .await
            .unwrap();
        let mut names: Vec<&str> = out
            .stdout
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .collect();
        names.sort();
        assert_eq!(names, ["GREETING", "HOME", "PATH"], "{}", out.stdout);
        assert!(out.stdout.contains("GREETING=hello"));
    }

    #[tokio::test]
    async fn exit_code_and_stderr_come_back() {
        let c = caller(&["/bin/sh"], &[]);
        let out = LocalShell::default()
            .run(
                &c,
                "/bin/sh".into(),
                strings(&["-c", "echo oops >&2; exit 3"]),
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stderr, "oops\n");
    }

    #[tokio::test]
    async fn a_slow_program_times_out() {
        let c = caller(&["/bin/sleep"], &[]);
        let shell = LocalShell {
            timeout: Duration::from_millis(100),
        };
        let r = shell
            .run(&c, "/bin/sleep".into(), strings(&["5"]), vec![])
            .await;
        assert_eq!(r, Err(CallError::Timeout));
    }
}
