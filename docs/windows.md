# Windows

The node compiles for Windows, with backends written for Windows 10 and 11. This page lists what each desktop capability does there, how the node runs as a logon task, what to install, and how to test. The code is in `crates/providers/src/windows/`, with the parts that need no Win32 in `crates/providers/src/win.rs`, so their tests run on every system.

None of this has run on a real Windows machine yet. `just check-windows` cross-compiles the workspace and runs clippy on it from macOS, and the unit tests cover the pure parts (key tables, the window filter, frame borders, the netstat parser, the task XML). Read every statement below as what the code does, not as something we watched work. The last section lists what still needs a real machine.

## What works where

The second column is a node with no desktop: started over SSH, or by a Windows service in session 0, which has no access to the user's desktop. We have not tried either; that column says what the Win32 calls are expected to do there.

| Capability | Windows desktop | No desktop (session 0, SSH) |
|---|---|---|
| `clipboard` | Yes (text, through arboard) | Expected to fail or see an empty clipboard |
| `hotkey` | Yes (`RegisterHotKey`) | Registers, but no key ever reaches it |
| `keyboard`, `mouse` | Yes (`SendInput`); `denied` when the focused app runs as administrator | Expected `denied`: there is no input desktop to send to |
| `window` | Yes | Empty list |
| `app.launch`, `app.open-url` | Yes (`ShellExecuteW`) | Starts the program in that session, where no one sees it |
| `app.running`, `app.frontmost`, `app.quit`, `app.set-hidden`, `app.watch` | Yes (from the Alt+Tab window list) | Empty list; `quit` and `set-hidden` answer "no such app" |
| `screen` | Yes (GDI) | Expected `failed` |
| `notify` | Yes (a toast) | Expected `failed` or `denied` |
| `selection` | `unavailable` | `unavailable` |
| `audio` | `unavailable` | `unavailable` |
| `mic`, `camera` | Yes, with ffmpeg (DirectShow) | Expected to find no devices |
| `system.power` | Yes | Yes |
| `system.idle-seconds` | Yes | Counts no input, so it keeps growing |
| `system.keep-awake` | Yes | Yes |
| `system.watch` | Yes | Sleep and wake only; no lock events |
| `secrets` | Yes (Credential Manager) | Yes when the user's profile is loaded |
| `tts` | Yes (SAPI 5, or a TTS server) | SAPI speaks if the session has a sound device |
| Everything else | Yes | Yes |

`bricolo service` starts the node in the user's own desktop session at logon (see below), so the first column is the one that applies to an installed node.

## Running the node

### The control pipe

There are no Unix sockets for the control channel, so `bricolo run` listens on a named pipe, `\\.\pipe\bricolo-<hash>`. The hash is 16 hex digits of FNV-1a over the data folder, lowercased and with `\` separators, so two nodes with different `BRICOLO_DATA_DIR` get different pipes. `bricolo status`, `bricolo fire` and the other commands, and `bricolo-ui`, compute the same name (`bricolo_runtime::paths::pipe_name`, mirrored in `crates/ui/src/client.rs`; both test the same value).

- The pipe's DACL grants the current user and nobody else (`crates/node/src/pipe_security.rs`). Without it a named pipe gives every local user read access.
- Remote clients are refused.
- A second `bricolo run` on the same data folder fails with "a node is already running".
- `bricolo run` stops on Ctrl+C or Ctrl+Break, when its console closes, and when the user logs off or Windows shuts down. After the last three Windows gives the process about 5 seconds.

### Sidecars

Windows has no signals. Each sidecar starts as the leader of its own console process group, and the node puts it in a job object that kills every process in it when the job closes, including when the node itself is killed.

- To stop a sidecar the node sends Ctrl+Break to its process group, waits 3 seconds, then kills the job, which also takes whatever the sidecar started.
- If the job object could not be made, it logs a warning and falls back to `taskkill /T /F /PID <pid>`.
- Before starting a sidecar, the node frees its port: it reads `netstat -ano -p TCP`, kills the processes listening on that port (never itself), and waits up to 5 seconds. A listener is told apart by its remote address (`0.0.0.0:0` or `[::]:0`), because the state column is translated on non-English Windows.

Ctrl+Break reaches a sidecar only if it shares the node's console. `bricolo-ui` starts `bricolo run` with a hidden console for that reason.

### Logon task

`bricolo service install` registers a Task Scheduler task named `bricolo` that runs at this user's logon, and runs it at once. With `--ui` it also registers `bricolo-ui`, which opens `bricolo-ui --background` at logon (it is not started at install).

- The task is registered from an XML file in `<data>\service\` with `schtasks /Create /XML ... /F`. `schtasks /SC ONLOGON /TR` would stop the task after 72 hours, would not start it on battery, and limits the command to 261 characters.
- It runs as the user, unelevated (`LeastPrivilege`), in the user's interactive session, with no time limit, on battery too.
- Its action is `conhost.exe --headless cmd.exe /d /s /c ""<data>\service\bricolo-service.cmd""`. `conhost --headless` gives the console program a console without a window.
- The wrapper `bricolo-service.cmd` sets `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR` when they were set at install, runs `bricolo run` with its output appended to `<data>\logs\bricolo-service.log`, and runs it again 5 seconds after it exits with an error. A clean exit (Ctrl+Break, logoff) ends it.
- `bricolo service start` and `stop` run `schtasks /Run` and `/End`. `stop` leaves the task registered, so the node starts again at the next logon. `status` prints the state lines of `schtasks /Query /V /FO LIST`.
- `bricolo service uninstall` ends and deletes both tasks and removes the wrappers and XML files.

See [install.md](install.md#windows).

## Desktop capabilities

### Windows and screens

`window` lists what Alt+Tab shows, front to back: visible, titled top-level windows that are not tool windows, not owned by another window and not cloaked. Cloaked windows include those on other virtual desktops, so those are left out.

- `id` is the HWND. `app` is the program's file name without `.exe` ("notepad", "Code").
- The node makes itself per-monitor DPI aware (v2) before the first call, so every coordinate is in physical pixels of the virtual screen, with 0,0 at the primary monitor's top-left corner. On macOS they are points.
- `frame` is DWM's visible bounds, so it leaves out the invisible resize borders Windows 10 and 11 draw around a window. `set-frame` adds them back, so the visible window lands where asked. It restores a maximized or minimized window first.
- `focus` restores a minimized window and brings it to the front. Windows lets a process take the foreground only if it got the last input, so when the plain call is refused the node taps Alt and attaches to the foreground thread's input, the usual workaround. If Windows still refuses, the call answers success anyway and logs it at debug level, with the other window still in front.
- `close` posts `WM_CLOSE`, as the close button does.
- `screens` comes from `EnumDisplayMonitors`. `visible-frame` is the work area, the monitor without the taskbar, and `main` is the primary monitor.

### Apps

An app is a program with a window in the Alt+Tab list, named by its file name without `.exe`. Names match in any case, with or without `.exe`. `bundle-id` is always empty.

- `launch` brings a running app of that name to the front, as `open -a` does. Otherwise it starts the first of: a Start Menu shortcut of that name (the user's, then everyone's), the program an App Paths entry names (HKCU, then HKLM), or `<name>.exe` on `PATH`. It goes through `ShellExecuteW`.
- `open-url` hands the URL to `ShellExecuteW`. `file:` URLs answer `denied`, as on macOS and Linux.
- `quit` posts `WM_CLOSE` to each of the app's windows, so the app can save or ask first; it never kills the process. `set-hidden` minimizes every window, or restores the minimized ones and brings the frontmost back. Both answer `failed` with "no such app" when no window matches.
- `watch` reports `activated` as soon as an app comes to the front (a WinEvent hook on `EVENT_SYSTEM_FOREGROUND`). A one-second timer compares the apps with windows to report `launched` (when its first window shows) and `terminated` (when its process exits), so those arrive within about a second.

### Hotkeys

`hotkey` registers each chord with `RegisterHotKey` on a thread of its own, which runs the message loop the hotkeys arrive on.

- `cmd` is the Windows key, `alt` is Alt, `ctrl` and `shift` are themselves.
- Letters, digits and punctuation are physical positions on a US keyboard, as for `keyboard`: `cmd+alt+q` is the key where Q sits on a US keyboard, whatever the layout.
- Holding a chord sends one press (`MOD_NOREPEAT`).
- Windows has no release message for a hotkey, so after a press the thread polls the key every 15 ms and sends `hotkey-released` when it goes up.
- A chord another program holds, or one Windows keeps for itself such as Win+L, answers `failed`.
- `fn` answers `unavailable`: the Fn key never reaches Windows.

### Keyboard and mouse

`keyboard` sends input through `SendInput`.

- `keystroke` presses keys by their scan codes, so key names are physical positions on a US keyboard, whatever the layout. `cmd` is the Windows key.
- `type-text` sends each character as itself (`KEYEVENTF_UNICODE`), so any text types on any layout. Line breaks and tabs press Enter and Tab.
- Modifiers the user still holds when the call starts, such as the chord that triggered it, are released first.
- Windows drops synthetic input aimed at an app that runs as administrator when the node does not. `SendInput` then inserts nothing, and the call answers `denied`.

`mouse` uses physical pixels of the virtual screen, like `window`. `scroll` sends one wheel notch per line asked for, and Windows scrolls each notch by what the user set in Mouse settings, three lines by default. So `scroll(0, 1)` moves about three lines on Windows and one elsewhere.

### Clipboard

`clipboard` is arboard on the Win32 clipboard, text only (`CF_UNICODETEXT`). Each call opens the clipboard on a blocking thread and closes it again; Windows keeps a copy of what was set, so nothing has to stay alive. arboard retries while another program holds the clipboard open. `selection` answers `unavailable` on Windows.

### Screen

`screen.capture` copies the screen with GDI (`BitBlt` with `CAPTUREBLT`, read back with `GetDIBits`) and writes a PNG to the plugin's data folder.

- With no area it captures the whole virtual screen, every monitor. macOS captures only the main screen.
- An area is in the coordinates `window.screens` reports and is clipped to the virtual screen.
- Windows asks for no permission.
- On the secure desktop (a UAC prompt, the lock screen) the copy fails and the call answers `failed`.

### Notifications

`notify` shows a toast through the WinRT `ToastNotificationManager`. Windows shows a toast only for an app it knows by AppUserModelID, and the node is not a packaged app. On the first call it registers `com.pierre-mike.bricolo` under `HKCU\Software\Classes\AppUserModelId\` with the display name "bricolo". That needs no Start Menu shortcut and no administrator rights.

- If Windows still does not know the ID, the toast goes out under Windows PowerShell's ID, and shows "Windows PowerShell" as its sender.
- If the user turned toasts off, for the app or for the whole system, the call answers `denied`. Focus assist does not count.

## Microphone and camera

`mic` and `camera` run ffmpeg with DirectShow (`-f dshow`).

- DirectShow has no "default" device. `[mic] device` and `[camera] device` in node.toml name one, with or without the `audio=` or `video=` prefix. Without one, the node asks ffmpeg for the list (`-list_devices true -f dshow -i dummy`) once per node run: `mic` tries each microphone in turn, and `camera` takes the first camera. A device plugged in later needs a node restart, or a `device` in node.toml.
- To stop a recording the node sends Ctrl+Break to ffmpeg's process group, so ffmpeg finishes the WAV file, then kills it if it has not exited.
- The node looks for `ffmpeg.exe` on `PATH`, or uses `[mic] ffmpeg`.

`audio` answers `unavailable`: ffmpeg has no Windows audio output (no WASAPI or DirectSound muxer).

## System

- `idle-seconds` is `GetLastInputInfo` against `GetTickCount`, for the session the node runs in.
- `power` is `GetSystemPowerStatus`.
- `keep-awake` calls `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)`. The state belongs to the thread that set it, so one thread, `bricolo-keep-awake`, holds it while any plugin asks for it. Like `caffeinate -i`, it stops idle sleep only: the display may still turn off, and a sleep the user asks for still happens.
- `watch` runs a hidden window on the thread `bricolo-system-watch`. `WM_POWERBROADCAST` gives `will-sleep` and `did-wake`, `WM_WTSSESSION_CHANGE` gives `screen-locked` and `screen-unlocked`, and `WM_DISPLAYCHANGE` gives `screens-changed`.

## Secrets

`secrets` keeps each value as a generic credential in the Credential Manager of the user the node runs as. The target name is `bricolo/<plugin>/<name>`, the user name is `<plugin>/<name>`, and the value is the secret's UTF-8 bytes. Credentials persist on this machine (`CRED_PERSIST_LOCAL_MACHINE`), not in a roaming profile. Windows limits a credential to 2560 bytes; a longer secret answers `failed`. `cmdkey /list:bricolo/*` lists them.

## Shell

`shell` starts programs without a shell, as elsewhere. Windows programs need more of the system's variables to start, so children get `SystemRoot`, `windir`, `ComSpec`, `PATHEXT`, `TEMP`, `TMP`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `ProgramData` and `ProgramFiles` from the node, and a `PATH` of `%SystemRoot%\System32`, `%SystemRoot%`, `System32\Wbem` and `System32\WindowsPowerShell\v1.0`. A plugin may not set any of these; Windows variable names ignore case, and so does the check. The child's working folder is `/`, the root of the current drive.

`~/` in `disk`, `files`, `audio`, `speech` and `mic` paths expands to `%USERPROFILE%`.

## TTS

With no TTS server in `node.toml`, the node speaks with SAPI 5 on a thread of its own, `bricolo-sapi`, with the same queue as elsewhere: one chunk at a time, and `stop` purges the speech mid-chunk. The voice is matched by name among the installed SAPI voices, the same name in any case, else the first name that contains it, so `"zira"` finds "Microsoft Zira Desktop". SAPI lists the voices under `HKLM\SOFTWARE\Microsoft\Speech\Voices`. The Windows 11 natural voices of Narrator are not SAPI voices, so they may be missing.

With a TTS server, the node plays its WAV files with `ffplay` if it is on `PATH`, else with Windows PowerShell's `System.Media.SoundPlayer`, which every Windows has and which plays WAV only. `[tts] player` overrides it.

## The hub

`bricolo-ui` has the code for the pipe, and a release build opens no console window. It is not built or checked on Windows yet: `just check-windows` and the Windows CI job leave it out, because its WebView2 build is not set up.

## Packages

Building needs the Rust toolchain from `rust-toolchain.toml` and a C compiler for the vendored Lua and `ring`: the MSVC build tools, or MinGW for `x86_64-pc-windows-gnu`.

At run time, install what you use:

| For | Install |
|---|---|
| `mic`, `camera` | ffmpeg, on `PATH` (`winget install Gyan.FFmpeg`) |
| `tts` with a server, beyond WAV | `ffplay`, which comes with ffmpeg |
| `tts` without a server | Nothing; more SAPI voices come from Settings, Time & language, Speech |

## Tests

The Windows desktop tests are in `crates/providers/tests/windows_desktop.rs`. They are `#[ignore]` and act on the signed-in user's desktop: the clipboard, a toast, a throwaway `bricolo-test` credential, the speakers, power and session state, and Notepad (opened, typed into, moved, captured and closed). Close Notepad first, then run them one at a time on a machine you can spare for a minute:

```sh
cargo nextest run -p bricolo-providers --test windows_desktop --run-ignored only --test-threads 1
```

`win_system_watch_sees_a_lock_or_a_display_change` waits for you to lock the screen or change the display.

CI has a `windows` job on `windows-latest` that builds the plugins and fixtures, then runs clippy and `cargo nextest run --workspace --exclude bricolo-ui`. It does not run the ignored desktop tests.

### From macOS

`just check-windows` runs clippy for `x86_64-pc-windows-gnu`, without `bricolo-ui`. It needs the target and zig, which `scripts/windows-cc` and `scripts/windows-ar` use as the C compiler and archiver:

```sh
rustup target add x86_64-pc-windows-gnu
brew install zig
just check-windows
```

This checks that the code compiles. It runs nothing.

### On a virtual machine

On an Apple silicon Mac, UTM and Parallels run Windows 11 for ARM. It runs x86_64 programs under emulation, so an `x86_64-pc-windows-gnu` build works there. To build natively inside the VM, install Rust with the MSVC build tools and build for `aarch64-pc-windows-msvc`, the default target there.

In the VM, install ffmpeg, build with `cargo build -p bricolo-node`, run the desktop tests above, then the live check: install `examples/apps/live-check/live_check.lua` (`bricolo install examples/apps/live-check/live_check.lua`), start `bricolo run`, focus a window you can spare and press ctrl+alt+shift+Win+L. The port is done when `bricolo logs` shows no `live_check: FAIL` line; `UNAVAILABLE` marks calls Windows does not have (`selection`, `audio`). Windows 10 and 11 may keep ctrl+alt+shift+Win chords for Office shortcuts (with L for LinkedIn); if the live check's chords answer `failed`, change `HYPER` at the top of the script.

## Not yet verified on a real machine

Everything above. In particular:

- The control pipe's DACL, and `bricolo-ui` reaching the node through it.
- Ctrl+Break reaching sidecars and ffmpeg, the job object killing what a sidecar left, and `netstat` parsing on a non-English Windows.
- The logon task: `conhost --headless`, the wrapper's restart, the log file, and `schtasks` accepting the XML.
- Toasts under the registered AUMID, and the fallback to PowerShell's.
- The foreground workaround in `focus`, `set-frame` with the border compensation on scaled monitors, and windows on other virtual desktops staying out of the list.
- Hotkey releases by polling, and which chords Windows and Office keep.
- `SendInput` with the US scan code table on other layouts, and `denied` for elevated apps.
- SAPI voices and `stop` mid-chunk; ffplay and `SoundPlayer` for a TTS server.
- DirectShow device names from real ffmpeg builds, and `mic` recording a complete WAV.
- GDI capture across monitors with different scaling.
- The second column of the table: a node with no desktop.
- `bricolo-ui` on Windows at all.
