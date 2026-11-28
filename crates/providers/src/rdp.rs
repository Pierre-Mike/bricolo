//! `virtual-screen` as a Remote Desktop session, on Windows Server: the
//! plans, free of Win32, so the tests check them on any system.
//!
//! Client editions of Windows allow one interactive session, so a screen of
//! its own needs Windows Server (with the Remote Desktop Session Host role
//! or its two administrative sessions). There, a loopback connection to
//! `127.0.0.2` signs a dedicated local account into a session of its own,
//! which no monitor shows. The node then works inside that session the only
//! way a process that is not SYSTEM can: one-shot scheduled tasks that run
//! as that account, interactively (`schtasks /it`), in its session.
//!
//! - The account: the user makes it once, adds it to Remote Desktop Users,
//!   and stores its name and password in the Credential Manager as the
//!   secrets `virtual-screen-rdp-user` and `virtual-screen-rdp-password`
//!   (`cmdkey /generic:bricolo/virtual-screen-rdp-user
//!   /user:bricolo /pass:<account name>`, the same for the password). The node
//!   never creates accounts.
//! - `start` stores the connection's credentials for `mstsc`
//!   (`cmdkey /generic:TERMSRV/127.0.0.2`), sets this user's
//!   `AuthenticationLevelOverride` to 0 while it connects (the machine's
//!   own certificate is self-signed; the old value comes back once the
//!   session starts or fails to), starts `mstsc /v:127.0.0.2`
//!   at the size asked (no `.rdp` file: opening one shows a warning that
//!   waits for a person), and waits for the account's session to show as
//!   Active in `query session`.
//! - `launch`, `capture`, `click`, `type-text` and `press-key` each run a
//!   short PowerShell script as a task in that session; scripts answer
//!   through files in a folder both accounts can read.
//! - `stop` signs the session out (`logoff`), ends `mstsc` and removes the
//!   stored connection credentials.
//!
//! `schtasks /rp` and `cmdkey /pass` take the password on their command
//! lines, where other processes of this machine can read it while they
//! run; use an account made for this alone.

use std::time::Duration;

use crate::cmd::{Ran, Runner};
use crate::error::CallError;

/// The loopback address a Remote Desktop client may use for this machine.
pub const HOST: &str = "127.0.0.2";
const TIMEOUT: Duration = Duration::from_secs(60);

/// The account a session runs as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub user: String,
    pub password: String,
}

/// `mstsc`'s arguments for a session of `width` by `height`. A command
/// line, not an `.rdp` file: since the April 2026 Windows update, opening
/// an `.rdp` file shows a warning that waits for a person to tick a box,
/// and the session never starts. The sign-in comes from the credentials
/// stored for [`HOST`].
pub fn mstsc_args(width: u32, height: u32) -> Vec<String> {
    vec![
        format!("/v:{HOST}"),
        format!("/w:{width}"),
        format!("/h:{height}"),
    ]
}

/// The id of `user`'s Active session in `query session` output.
pub fn active_session(output: &str, user: &str) -> Option<u32> {
    output.lines().skip(1).find_map(|line| {
        let line = line.trim_start_matches(['>', ' ']);
        let parts: Vec<&str> = line.split_whitespace().collect();
        // SESSIONNAME USERNAME ID STATE, or USERNAME ID STATE when the
        // session has no name.
        let at = parts.iter().position(|p| p.eq_ignore_ascii_case(user))?;
        let id: u32 = parts.get(at + 1)?.parse().ok()?;
        let state = parts.get(at + 2)?;
        state.eq_ignore_ascii_case("Active").then_some(id)
    })
}

/// Where `mstsc` keeps this user's settings.
const CLIENT_KEY: &str = r"HKCU\Software\Microsoft\Terminal Server Client";
const AUTH_OVERRIDE: &str = "AuthenticationLevelOverride";

/// The value of a `REG_DWORD` in `reg query` output, like `0x2`.
pub fn reg_dword(output: &str, name: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != name || words.next()? != "REG_DWORD" {
            return None;
        }
        u32::from_str_radix(words.next()?.trim_start_matches("0x"), 16).ok()
    })
}

/// Whether `reg query … /v InstallationType` says Windows Server.
pub fn is_server(output: &str) -> bool {
    output.contains("Server")
}

/// PowerShell for the actions, run as a task inside the session. `{OUT}`
/// is where the script writes its answer.
pub fn launch_script(program: &str, args: &[String], out: &str) -> String {
    let quoted: Vec<String> = args.iter().map(|a| ps_quote(a)).collect();
    let arg_list = if quoted.is_empty() {
        String::new()
    } else {
        format!(" -ArgumentList @({})", quoted.join(", "))
    };
    format!(
        "$p = Start-Process -FilePath {}{arg_list} -PassThru\nSet-Content -Path {} -Value $p.Id\n",
        ps_quote(program),
        ps_quote(out)
    )
}

pub fn capture_script(width: u32, height: u32, out: &str) -> String {
    format!(
        "Add-Type -AssemblyName System.Drawing\n\
         $b = New-Object System.Drawing.Bitmap {width}, {height}\n\
         $g = [System.Drawing.Graphics]::FromImage($b)\n\
         $g.CopyFromScreen(0, 0, 0, 0, $b.Size)\n\
         $b.Save({}, [System.Drawing.Imaging.ImageFormat]::Png)\n",
        ps_quote(out)
    )
}

const USER32: &str = "Add-Type -Namespace Rh -Name U -MemberDefinition '[DllImport(\"user32.dll\")] public static extern bool SetCursorPos(int x, int y); [DllImport(\"user32.dll\")] public static extern void mouse_event(int f, int x, int y, int d, int e);'\n";

pub fn click_script(x: i32, y: i32) -> String {
    format!(
        "{USER32}[Rh.U]::SetCursorPos({x}, {y}) | Out-Null\n[Rh.U]::mouse_event(2, 0, 0, 0, 0)\n[Rh.U]::mouse_event(4, 0, 0, 0, 0)\n"
    )
}

/// `SendKeys` text: its special characters go in braces.
pub fn send_keys_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '+' | '^' | '%' | '~' | '(' | ')' | '{' | '}' | '[' | ']' => {
                out.push('{');
                out.push(c);
                out.push('}');
            }
            '\n' => out.push_str("{ENTER}"),
            '\r' => {}
            '\t' => out.push_str("{TAB}"),
            c => out.push(c),
        }
    }
    out
}

/// `SendKeys` for a chord: `^` ctrl, `%` alt, `+` shift; the Windows key
/// has no `SendKeys` form.
pub fn send_keys_chord(chord: &crate::hotkey::Chord) -> Option<String> {
    use crate::hotkey::Modifier;
    let mut out = String::new();
    for m in &chord.mods {
        out.push_str(match m {
            Modifier::Ctrl => "^",
            Modifier::Alt => "%",
            Modifier::Shift => "+",
            Modifier::Cmd => return None,
        });
    }
    let key = match chord.code.as_str() {
        "Enter" => "{ENTER}".to_string(),
        "Escape" => "{ESC}".into(),
        "Tab" => "{TAB}".into(),
        "Backspace" => "{BACKSPACE}".into(),
        "Delete" => "{DELETE}".into(),
        "ArrowLeft" => "{LEFT}".into(),
        "ArrowRight" => "{RIGHT}".into(),
        "ArrowUp" => "{UP}".into(),
        "ArrowDown" => "{DOWN}".into(),
        "Home" => "{HOME}".into(),
        "End" => "{END}".into(),
        "PageUp" => "{PGUP}".into(),
        "PageDown" => "{PGDN}".into(),
        "Space" => " ".into(),
        c => {
            if let Some(l) = c.strip_prefix("Key") {
                l.to_lowercase()
            } else if let Some(d) = c.strip_prefix("Digit") {
                d.to_string()
            } else {
                format!("{{F{}}}", c.strip_prefix('F')?)
            }
        }
    };
    out.push_str(&key);
    Some(out)
}

pub fn keys_script(keys: &str) -> String {
    format!(
        "Add-Type -AssemblyName System.Windows.Forms\n[System.Windows.Forms.SendKeys]::SendWait({})\n",
        ps_quote(keys)
    )
}

/// A PowerShell single-quoted string.
pub fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Runs the plans through a [`Runner`].
pub struct Plans<'a, R: Runner> {
    pub runner: &'a R,
}

fn ok(r: Ran, what: &str) -> Result<String, CallError> {
    r.text(what)
}

impl<R: Runner> Plans<'_, R> {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, CallError> {
        let r = self.runner.run(program, args, None, TIMEOUT)?;
        ok(r, program)
    }

    /// `unavailable` off Windows Server.
    pub fn check_server(&self) -> Result<(), CallError> {
        let out = self.run(
            "reg",
            &[
                "query",
                r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
                "/v",
                "InstallationType",
            ],
        )?;
        if is_server(&out) {
            Ok(())
        } else {
            Err(CallError::Unavailable)
        }
    }

    pub fn store_credentials(&self, a: &Account) -> Result<(), CallError> {
        let target = format!("/generic:TERMSRV/{HOST}");
        let user = format!("/user:{}", a.user);
        let pass = format!("/pass:{}", a.password);
        self.run("cmdkey", &[&target, &user, &pass]).map(drop)
    }

    /// Lets `mstsc` connect without asking about the certificate and
    /// returns the setting it replaced. The machine's own Remote Desktop
    /// certificate is self-signed, so `mstsc` stops at "The identity of the
    /// remote computer cannot be verified", and a stored `CertHash` for
    /// [`HOST`] does not stop it. The setting holds for every connection
    /// this user makes, so [`Self::restore_authentication`] puts it back as
    /// soon as the session starts or fails to.
    pub fn lower_authentication(&self) -> Result<Option<u32>, CallError> {
        let r = self.runner.run(
            "reg",
            &["query", CLIENT_KEY, "/v", AUTH_OVERRIDE],
            None,
            TIMEOUT,
        )?;
        let before = if r.ok() {
            reg_dword(&r.stdout, AUTH_OVERRIDE)
        } else {
            None
        };
        self.set_authentication(0)?;
        Ok(before)
    }

    /// Puts back what [`Self::lower_authentication`] replaced.
    pub fn restore_authentication(&self, before: Option<u32>) {
        let _ = match before {
            Some(v) => self.set_authentication(v),
            None => self
                .run("reg", &["delete", CLIENT_KEY, "/v", AUTH_OVERRIDE, "/f"])
                .map(drop),
        };
    }

    fn set_authentication(&self, level: u32) -> Result<(), CallError> {
        let level = level.to_string();
        self.run(
            "reg",
            &[
                "add",
                CLIENT_KEY,
                "/v",
                AUTH_OVERRIDE,
                "/t",
                "REG_DWORD",
                "/d",
                &level,
                "/f",
            ],
        )
        .map(drop)
    }

    pub fn forget_credentials(&self) {
        let _ = self.run("cmdkey", &[&format!("/delete:TERMSRV/{HOST}")]);
    }

    /// `query session` can exit 1 while it prints the whole table, as on
    /// the hosted Windows Server runners, so the table wins over the code.
    pub fn session(&self, user: &str) -> Result<Option<u32>, CallError> {
        let r = self.runner.run("query", &["session"], None, TIMEOUT)?;
        if r.stdout.contains("SESSIONNAME") {
            return Ok(active_session(&r.stdout, user));
        }
        ok(r, "query").map(|out| active_session(&out, user))
    }

    /// Runs `script` as a task in `user`'s session and waits for it.
    pub fn in_session(&self, a: &Account, name: &str, script_path: &str) -> Result<(), CallError> {
        let command = format!(
            "powershell.exe -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File \"{script_path}\""
        );
        self.run(
            "schtasks",
            &[
                "/create",
                "/f",
                "/tn",
                name,
                "/sc",
                "once",
                "/st",
                "00:00",
                "/it",
                "/ru",
                &a.user,
                "/rp",
                &a.password,
                "/tr",
                &command,
            ],
        )?;
        self.run("schtasks", &["/run", "/tn", name]).map(drop)
    }

    pub fn delete_task(&self, name: &str) {
        let _ = self.run("schtasks", &["/delete", "/f", "/tn", name]);
    }

    pub fn logoff(&self, session: u32) {
        let _ = self.run("logoff", &[&session.to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::FakeRunner;
    use crate::hotkey::Chord;

    fn account() -> Account {
        Account {
            user: "bricolo-agent".into(),
            password: "s3cret-pw".into(),
        }
    }

    #[test]
    fn reads_a_dword_from_reg_query() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Terminal Server Client\r\n    AuthenticationLevelOverride    REG_DWORD    0x2\r\n";
        assert_eq!(reg_dword(out, "AuthenticationLevelOverride"), Some(2));
        assert_eq!(reg_dword(out, "Other"), None);
        assert_eq!(reg_dword("", "AuthenticationLevelOverride"), None);
    }

    #[test]
    fn mstsc_connects_with_the_size_asked() {
        assert_eq!(mstsc_args(1280, 800), ["/v:127.0.0.2", "/w:1280", "/h:800"]);
    }

    #[test]
    fn finds_the_accounts_active_session() {
        let out = " SESSIONNAME       USERNAME                 ID  STATE   TYPE        DEVICE\n\
                    services                                    0  Disc\n\
                   >console           admin                     1  Active\n\
                    rdp-tcp#3         bricolo-agent                  3  Active\n\
                                      other                     4  Disc\n";
        assert_eq!(active_session(out, "bricolo-agent"), Some(3));
        assert_eq!(active_session(out, "BRICOLO-AGENT"), Some(3));
        assert_eq!(active_session(out, "other"), None);
        assert_eq!(active_session(out, "nobody"), None);
        assert!(is_server("    InstallationType    REG_SZ    Server"));
        assert!(!is_server("    InstallationType    REG_SZ    Client"));
    }

    #[test]
    fn a_session_table_counts_whatever_the_exit_code() {
        let r = FakeRunner::default();
        r.answers.lock().unwrap().push((
            "query session".into(),
            Ok(crate::cmd::Ran {
                code: Some(1),
                stdout: " SESSIONNAME  USERNAME  ID  STATE\r\n>console  runneradmin  2  Active\r\n rdp-tcp  65537  Listen\r\n".into(),
                stderr: String::new(),
            }),
        ));
        let p = Plans { runner: &r };
        assert_eq!(p.session("bricolo-agent").unwrap(), None);
        assert_eq!(p.session("runneradmin").unwrap(), Some(2));

        let broken = FakeRunner::default();
        broken.fail("query session", 1, "No session exists");
        let p = Plans { runner: &broken };
        assert!(p.session("bricolo-agent").is_err());
    }

    #[test]
    fn scripts_quote_their_arguments() {
        let s = launch_script(
            r"C:\Program Files\App\app.exe",
            &["it's".into(), "b".into()],
            r"C:\Users\Public\o.txt",
        );
        assert!(s.contains(r"Start-Process -FilePath 'C:\Program Files\App\app.exe' -ArgumentList @('it''s', 'b') -PassThru"));
        assert!(s.contains(r"Set-Content -Path 'C:\Users\Public\o.txt' -Value $p.Id"));
        assert!(launch_script("a.exe", &[], "o").contains("-FilePath 'a.exe' -PassThru"));
        assert!(
            capture_script(800, 600, "s.png").contains("New-Object System.Drawing.Bitmap 800, 600")
        );
        assert!(click_script(10, 20).contains("SetCursorPos(10, 20)"));
        assert_eq!(send_keys_text("a+b {x}\n"), "a{+}b {{}x{}}{ENTER}");
    }

    #[test]
    fn chords_become_send_keys() {
        let k = |s: &str| send_keys_chord(&Chord::parse(s).unwrap());
        assert_eq!(k("ctrl+s").as_deref(), Some("^s"));
        assert_eq!(k("ctrl+shift+f5").as_deref(), Some("^+{F5}"));
        assert_eq!(k("return").as_deref(), Some("{ENTER}"));
        assert_eq!(k("cmd+l"), None);
        assert!(keys_script("^s").contains("SendWait('^s')"));
    }

    #[test]
    fn plans_run_the_commands() {
        let r = FakeRunner::default();
        r.answer(
            r"reg query HKLM",
            "    InstallationType    REG_SZ    Server Core",
        )
        .answer(
            "query session",
            "SESSIONNAME USERNAME ID STATE\nrdp-tcp#1 bricolo-agent 2 Active\n",
        );
        r.answer(
            r"reg query HKCU\Software\Microsoft\Terminal Server Client /v",
            "    AuthenticationLevelOverride    REG_DWORD    0x2\r\n",
        );
        let p = Plans { runner: &r };
        p.check_server().unwrap();
        p.store_credentials(&account()).unwrap();
        assert_eq!(p.lower_authentication().unwrap(), Some(2));
        p.restore_authentication(Some(2));
        p.restore_authentication(None);
        assert_eq!(p.session("bricolo-agent").unwrap(), Some(2));
        p.in_session(&account(), "bricolo-vs-1-1", r"C:\t\a.ps1")
            .unwrap();
        p.delete_task("bricolo-vs-1-1");
        p.logoff(2);
        p.forget_credentials();
        let calls = r.calls();
        assert!(calls.contains(
            &"cmdkey /generic:TERMSRV/127.0.0.2 /user:bricolo-agent /pass:s3cret-pw".to_string()
        ));
        assert!(calls.iter().any(|c| c.starts_with("schtasks /create /f /tn bricolo-vs-1-1 /sc once /st 00:00 /it /ru bricolo-agent /rp s3cret-pw /tr powershell.exe")));
        assert!(calls.contains(&"schtasks /run /tn bricolo-vs-1-1".to_string()));
        for call in [
            r"reg add HKCU\Software\Microsoft\Terminal Server Client /v AuthenticationLevelOverride /t REG_DWORD /d 0 /f",
            r"reg add HKCU\Software\Microsoft\Terminal Server Client /v AuthenticationLevelOverride /t REG_DWORD /d 2 /f",
            r"reg delete HKCU\Software\Microsoft\Terminal Server Client /v AuthenticationLevelOverride /f",
        ] {
            assert!(calls.contains(&call.to_string()), "{call} in {calls:?}");
        }
        assert!(calls.contains(&"logoff 2".to_string()));
        assert!(calls.contains(&"cmdkey /delete:TERMSRV/127.0.0.2".to_string()));

        let client = FakeRunner::default();
        client.answer("reg query", "    InstallationType    REG_SZ    Client");
        assert_eq!(
            Plans { runner: &client }.check_server(),
            Err(CallError::Unavailable)
        );
    }
}
