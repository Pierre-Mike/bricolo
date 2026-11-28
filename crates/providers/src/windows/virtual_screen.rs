//! `virtual-screen` on Windows: a Remote Desktop session of its own on
//! Windows Server when an account for it is stored ([`WindowsScreens`],
//! the plans in `rdp.rs`), else one hidden desktop per screen.
//!
//! The goal was a screen in its own RDP session, which gives a complete
//! second desktop that apps, input and screenshots all reach. That is not
//! available here. A loopback Remote Desktop connection to the same
//! machine needs the RDP host role, and client editions of Windows (Home,
//! Pro, Enterprise) allow one interactive session at a time: connecting
//! signs the console user out, or is refused. The workarounds (the
//! "RDP Wrapper" project and patched `termsrv.dll`) are third-party
//! patches to a system file, which this node does not install. So the
//! hidden desktop is the first step: a Win32 desktop object, made with
//! `CreateDesktopW`, that no monitor shows and that the signed-in user's
//! window station keeps alongside the visible one.
//!
//! - `start` makes the desktop. `launch` starts a program on it
//!   (`STARTUPINFOW.lpDesktop` names the desktop, so the program's windows
//!   open there). `alive` reports which launched processes still run, and
//!   `stop` ends them and closes the desktop.
//! - `capture` asks each visible window on the desktop to paint itself
//!   (`PrintWindow`, full content) and composes the pictures, bottom window
//!   first, on a white canvas of the screen's size.
//! - `click`, `type_text` and `press_key` post window messages, since
//!   `SendInput` reaches only the input desktop, the one the user sees. A
//!   click goes to the deepest visible child under the point
//!   (`WM_LBUTTONDOWN`, `WM_LBUTTONUP`) and becomes where text goes. Text
//!   is `WM_CHAR` to that window or, before any click, to the first edit
//!   control of the topmost window. Keys are `WM_KEYDOWN` and `WM_KEYUP`;
//!   chords with modifiers are refused, because a posted message carries
//!   no modifier state.
//! - `width` and `height` set the picture's size. Windows still open at
//!   the session's resolution: a hidden desktop has no display of its own.
//!
//! Apps that draw with DirectX or ignore `PrintWindow` can come out blank,
//! and apps that read the keyboard state rather than messages can ignore
//! posted keys.
//!
//! A program started here can also be seen from `accessibility` only if
//! the node's own thread is on that desktop, which it is not; the engine
//! lists the apps of the input desktop.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
    ScreenToClient, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, DESKTOP_CONTROL_FLAGS, EnumDesktopWindows, HDESK,
};
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetExitCodeProcess, PROCESS_INFORMATION,
    STARTUPINFOW, TerminateProcess,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CWP_SKIPINVISIBLE, ChildWindowFromPointEx, EnumChildWindows, GetClassNameW, GetWindowRect,
    IsWindowVisible, PostMessageW, WM_CHAR, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
};
use windows::core::BOOL;
use windows::core::{PCWSTR, PWSTR};

use super::wm::failed;
use crate::error::CallError;
use crate::hotkey::Chord;
use crate::virtual_screen::ScreenBackend;
use crate::win;

/// `GENERIC_ALL`: the creator may do anything with the desktop.
const DESKTOP_ALL_ACCESS: u32 = 0x1000_0000;
/// `GetExitCodeProcess`'s answer for a process that still runs.
const STILL_ACTIVE: u32 = 0x103;

/// The backend. Names are numbered from 1 for this node.
#[derive(Default)]
pub struct HiddenDesktops {
    next: AtomicU32,
}

/// A running hidden desktop and the processes launched on it.
pub struct Desktop {
    handle: HDESK,
    name: String,
    width: u32,
    height: u32,
    processes: Vec<Launched>,
    /// The window the last click reached, where text goes next.
    focus: std::sync::Mutex<Option<isize>>,
}

/// `PW_RENDERFULLCONTENT`: windows that compose with DWM paint too.
const PW_RENDERFULLCONTENT: u32 = 2;
/// `MK_LBUTTON` in a button-down message's `WPARAM`.
const MK_LBUTTON: usize = 1;

struct Launched {
    pid: u32,
    handle: HANDLE,
}

// SAFETY: a desktop handle and process handles are plain kernel handles,
// valid on any thread; the engine serialises calls on one screen.
unsafe impl Send for Desktop {}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Launched {
    fn running(&self) -> bool {
        let mut code = 0u32;
        // SAFETY: a process handle this value owns.
        unsafe { GetExitCodeProcess(self.handle, &raw mut code) }.is_ok() && code == STILL_ACTIVE
    }

    fn close(self) {
        // SAFETY: closes the handle `launch` got, once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

impl ScreenBackend for HiddenDesktops {
    type Screen = Desktop;

    fn start(&self, width: u32, height: u32) -> Result<(Desktop, String), CallError> {
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let name = win::hidden_desktop_name(std::process::id(), n);
        let wide_name = wide(&name);
        // SAFETY: a NUL-terminated name that outlives the call; no device,
        // mode or security attributes (the desktop belongs to this user).
        let handle = unsafe {
            CreateDesktopW(
                PCWSTR(wide_name.as_ptr()),
                PCWSTR::null(),
                None,
                DESKTOP_CONTROL_FLAGS(0),
                DESKTOP_ALL_ACCESS,
                None,
            )
        }
        .map_err(failed)?;
        Ok((
            Desktop {
                handle,
                name: name.clone(),
                width,
                height,
                processes: Vec::new(),
                focus: std::sync::Mutex::new(None),
            },
            name,
        ))
    }

    fn launch(
        &self,
        screen: &mut Desktop,
        program: &str,
        args: &[String],
    ) -> Result<u32, CallError> {
        let mut command = wide(&win::command_line(program, args));
        let mut desktop = wide(&screen.name);
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: `command` and `desktop` are writable NUL-terminated
        // buffers that outlive the call (CreateProcessW may edit the
        // command line); `startup` and `info` are initialised structs.
        unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_UNICODE_ENVIRONMENT,
                None,
                PCWSTR::null(),
                &startup,
                &raw mut info,
            )
        }
        .map_err(|e| CallError::Failed(format!("cannot start {program}: {e}")))?;
        // The primary thread's handle is of no use.
        // SAFETY: closes the handle `CreateProcessW` returned.
        let _ = unsafe { CloseHandle(info.hThread) };
        screen.processes.push(Launched {
            pid: info.dwProcessId,
            handle: info.hProcess,
        });
        Ok(info.dwProcessId)
    }

    fn alive(&self, screen: &mut Desktop) -> Vec<u32> {
        let (running, gone): (Vec<Launched>, Vec<Launched>) = std::mem::take(&mut screen.processes)
            .into_iter()
            .partition(Launched::running);
        for g in gone {
            g.close();
        }
        let pids = running.iter().map(|p| p.pid).collect();
        screen.processes = running;
        pids
    }

    fn capture(&self, screen: &Desktop, path: &std::path::Path) -> Result<(), CallError> {
        let mut pictures = Vec::new();
        // Bottom window first, so the ones above paint over it.
        for (hwnd, r) in windows_on(screen.handle)?.into_iter().rev() {
            if let Some(bgrx) = paint(hwnd, r.right - r.left, r.bottom - r.top) {
                pictures.push((r, bgrx));
            }
        }
        let layers: Vec<win::Layer> = pictures
            .iter()
            .map(|(r, bgrx)| win::Layer {
                x: r.left,
                y: r.top,
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
                bgrx,
            })
            .collect();
        let rgba = win::compose(screen.width, screen.height, &layers);
        let image = crate::clipboard::Image {
            width: screen.width,
            height: screen.height,
            rgba,
        };
        let png = crate::clipboard::encode_png(&image)?;
        std::fs::write(path, png)
            .map_err(|e| CallError::Failed(format!("cannot write {}: {e}", path.display())))
    }

    fn click(&self, screen: &Desktop, x: i32, y: i32) -> Result<(), CallError> {
        if x < 0 || y < 0 || x >= screen.width as i32 || y >= screen.height as i32 {
            return Err(CallError::Failed(format!(
                "invalid_argument: {x},{y} is outside the {}x{} screen",
                screen.width, screen.height
            )));
        }
        let windows = windows_on(screen.handle)?;
        let frames: Vec<(i32, i32, i32, i32)> = windows
            .iter()
            .map(|(_, r)| (r.left, r.top, r.right - r.left, r.bottom - r.top))
            .collect();
        let Some(i) = win::window_at(&frames, x, y) else {
            let seen: Vec<String> = frames
                .iter()
                .map(|(l, t, w, h)| format!("{w}x{h} at {l},{t}"))
                .collect();
            return Err(CallError::Failed(format!(
                "no window at {x},{y}; windows: [{}]",
                seen.join(", ")
            )));
        };
        let target = deepest_child(windows[i].0, x, y);
        let mut p = POINT { x, y };
        // SAFETY: a window handle from this desktop and a local point.
        let _ = unsafe { ScreenToClient(target, &raw mut p) };
        let at = LPARAM(win::point_lparam(p.x, p.y));
        post(target, WM_LBUTTONDOWN, WPARAM(MK_LBUTTON), at)?;
        post(target, WM_LBUTTONUP, WPARAM(0), at)?;
        *screen.focus.lock().unwrap() = Some(target.0 as isize);
        Ok(())
    }

    fn type_text(&self, screen: &Desktop, text: &str) -> Result<(), CallError> {
        let target = text_target(screen)?;
        for unit in win::char_units(text) {
            post(target, WM_CHAR, WPARAM(unit as usize), LPARAM(1))?;
        }
        Ok(())
    }

    fn press_key(&self, screen: &Desktop, chord: &Chord) -> Result<(), CallError> {
        if !chord.mods.is_empty() {
            return Err(CallError::Failed(format!(
                "background_unavailable: {chord} needs modifiers held, which a posted message \
                 cannot carry on a hidden desktop"
            )));
        }
        let vk = win::virtual_key(&chord.code)
            .ok_or_else(|| CallError::Failed(format!("no Windows key for {}", chord.code)))?;
        let target = text_target(screen)?;
        post(target, WM_KEYDOWN, WPARAM(vk as usize), LPARAM(1))?;
        post(
            target,
            WM_KEYUP,
            WPARAM(vk as usize),
            LPARAM(0xC000_0001u32 as isize),
        )
    }

    fn stop(&self, screen: Desktop) {
        for p in screen.processes {
            // SAFETY: a process handle this value owns.
            let _ = unsafe { TerminateProcess(p.handle, 1) };
            p.close();
        }
        // SAFETY: closes the desktop handle `start` made, once. Windows
        // destroys the desktop when its last handle and process are gone.
        let _ = unsafe { CloseDesktop(screen.handle) };
    }
}

unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
    // SAFETY: `list` is the `Vec<HWND>` the caller passed, alive for the
    // whole enumeration.
    let list = unsafe { &mut *(list.0 as *mut Vec<HWND>) };
    list.push(hwnd);
    BOOL(1)
}

/// The visible windows on the desktop with a size, topmost first.
fn windows_on(desktop: HDESK) -> Result<Vec<(HWND, RECT)>, CallError> {
    let mut all: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `all`, which outlives the call.
    unsafe { EnumDesktopWindows(Some(desktop), Some(collect), LPARAM(&raw mut all as isize)) }
        .map_err(failed)?;
    Ok(all
        .into_iter()
        .filter_map(|h| {
            // SAFETY: plain reads of a window handle.
            if !unsafe { IsWindowVisible(h) }.as_bool() {
                return None;
            }
            let mut r = RECT::default();
            unsafe { GetWindowRect(h, &raw mut r) }.ok()?;
            (r.right > r.left && r.bottom > r.top).then_some((h, r))
        })
        .collect())
}

/// The window's own painting, as BGRX rows, or `None` when it would not.
fn paint(hwnd: HWND, w: i32, h: i32) -> Option<Vec<u8>> {
    // SAFETY: every GDI object made here is released before returning; the
    // buffer holds `w * h` 32-bit pixels, as the header says.
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory: HDC = CreateCompatibleDC(Some(screen));
        let bitmap: HBITMAP = CreateCompatibleBitmap(screen, w, h);
        let mut out = None;
        if !memory.is_invalid() && !bitmap.is_invalid() {
            let old: HGDIOBJ = SelectObject(memory, bitmap.into());
            let painted = PrintWindow(hwnd, memory, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT));
            SelectObject(memory, old);
            if painted.as_bool() {
                let mut info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        biHeight: -h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut pixels = vec![0u8; w as usize * h as usize * 4];
                let rows = GetDIBits(
                    memory,
                    bitmap,
                    0,
                    h as u32,
                    Some(pixels.as_mut_ptr().cast()),
                    &raw mut info,
                    DIB_RGB_COLORS,
                );
                if rows == h {
                    out = Some(pixels);
                }
            }
        }
        if !bitmap.is_invalid() {
            let _ = DeleteObject(bitmap.into());
        }
        if !memory.is_invalid() {
            let _ = DeleteDC(memory);
        }
        ReleaseDC(None, screen);
        out
    }
}

/// The deepest visible child of `top` under the screen point.
fn deepest_child(top: HWND, x: i32, y: i32) -> HWND {
    let mut at = top;
    for _ in 0..16 {
        let mut p = POINT { x, y };
        // SAFETY: plain calls on a window handle.
        let _ = unsafe { ScreenToClient(at, &raw mut p) };
        let child = unsafe { ChildWindowFromPointEx(at, p, CWP_SKIPINVISIBLE) };
        if child.is_invalid() || child == at {
            break;
        }
        at = child;
    }
    at
}

unsafe extern "system" fn first_edit(hwnd: HWND, found: LPARAM) -> BOOL {
    let mut class = [0u16; 64];
    // SAFETY: a stack buffer of the length given.
    let n = unsafe { GetClassNameW(hwnd, &mut class) };
    let name = String::from_utf16_lossy(&class[..n.max(0) as usize]);
    if win::is_edit_class(&name) {
        // SAFETY: `found` is the `Option<HWND>` the caller passed.
        unsafe { *(found.0 as *mut Option<HWND>) = Some(hwnd) };
        return BOOL(0);
    }
    BOOL(1)
}

/// Where text and keys go: the last clicked window, else the topmost
/// window's first edit control, else the topmost window.
fn text_target(screen: &Desktop) -> Result<HWND, CallError> {
    if let Some(h) = *screen.focus.lock().unwrap() {
        return Ok(HWND(h as *mut _));
    }
    let windows = windows_on(screen.handle)?;
    let Some((top, _)) = windows.first() else {
        return Err(CallError::Failed("no window on the virtual screen".into()));
    };
    let mut found: Option<HWND> = None;
    // SAFETY: the callback writes only into `found`, which outlives it.
    let _ = unsafe {
        EnumChildWindows(
            Some(*top),
            Some(first_edit),
            LPARAM(&raw mut found as isize),
        )
    };
    Ok(found.unwrap_or(*top))
}

fn post(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> Result<(), CallError> {
    // SAFETY: posting a message copies its arguments.
    unsafe { PostMessageW(Some(hwnd), msg, w, l) }.map_err(failed)
}

/// The Credential Manager secrets that name the account of RDP screens.
const RDP_USER: &str = "virtual-screen-rdp-user";
const RDP_PASSWORD: &str = "virtual-screen-rdp-password";
/// A folder both the node's user and the session's account can use.
const SHARED: &str = r"C:\Users\Public\bricolo-virtual-screen";

/// The Windows backend: an RDP session on Windows Server when the account
/// is stored, else a hidden desktop.
#[derive(Default)]
pub struct WindowsScreens {
    hidden: HiddenDesktops,
    next: AtomicU32,
}

pub enum Screen {
    Hidden(Desktop),
    Rdp(RdpScreen),
}

pub struct RdpScreen {
    account: crate::rdp::Account,
    session: u32,
    width: u32,
    height: u32,
    client: std::process::Child,
    dir: std::path::PathBuf,
    n: u32,
    tasks: AtomicU32,
    pids: Vec<u32>,
}

fn rdp_account() -> Option<crate::rdp::Account> {
    use crate::secrets::Store;
    let store = super::credentials::CredentialStore;
    let user = store.get(RDP_USER).ok()??;
    let password = store.get(RDP_PASSWORD).ok()??;
    Some(crate::rdp::Account { user, password })
}

fn plans() -> crate::rdp::Plans<'static, crate::cmd::SystemRunner> {
    static RUNNER: crate::cmd::SystemRunner = crate::cmd::SystemRunner;
    crate::rdp::Plans { runner: &RUNNER }
}

fn process_running(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: opens and closes a handle; reads its exit code.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let running = GetExitCodeProcess(h, &raw mut code).is_ok() && code == STILL_ACTIVE;
        let _ = CloseHandle(h);
        running
    }
}

impl RdpScreen {
    /// Runs `script` as a task in the session and, with `answer`, waits up
    /// to 20 s for the script to write that file.
    fn run(&self, script: &str, answer: Option<&std::path::Path>) -> Result<(), CallError> {
        let t = self.tasks.fetch_add(1, Ordering::Relaxed) + 1;
        let name = format!(
            "bricolo-virtual-screen-{}-{}-{t}",
            std::process::id(),
            self.n
        );
        let file = self.dir.join(format!("{t}.ps1"));
        std::fs::write(&file, script).map_err(CallError::failed)?;
        let p = plans();
        let ran = p.in_session(&self.account, &name, &file.display().to_string());
        let mut result = ran;
        if result.is_ok()
            && let Some(answer) = answer
        {
            let start = std::time::Instant::now();
            while !answer.exists() {
                if start.elapsed() > std::time::Duration::from_secs(20) {
                    result = Err(CallError::Timeout);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // The script may still be writing: give it a moment.
            std::thread::sleep(std::time::Duration::from_millis(200));
        } else if result.is_ok() {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        p.delete_task(&name);
        let _ = std::fs::remove_file(&file);
        result
    }
}

impl ScreenBackend for WindowsScreens {
    type Screen = Screen;

    fn start(&self, width: u32, height: u32) -> Result<(Screen, String), CallError> {
        let Some(account) = rdp_account() else {
            let (d, name) = self.hidden.start(width, height)?;
            return Ok((Screen::Hidden(d), name));
        };
        let p = plans();
        if p.check_server().is_err() {
            let (d, name) = self.hidden.start(width, height)?;
            return Ok((Screen::Hidden(d), name));
        }
        if p.session(&account.user)?.is_some() {
            return Err(CallError::Failed(format!(
                "{} already has a session; one RDP screen per account",
                account.user
            )));
        }
        let n = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let dir = std::path::Path::new(SHARED).join(format!("{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(CallError::failed)?;
        p.store_credentials(&account)?;
        p.trust_certificate()?;
        let mut client = std::process::Command::new("mstsc.exe")
            .args(crate::rdp::mstsc_args(width, height))
            .spawn()
            .map_err(|e| CallError::Failed(format!("cannot start mstsc: {e}")))?;
        let start = std::time::Instant::now();
        let session = loop {
            if let Some(id) = p.session(&account.user)? {
                break id;
            }
            if start.elapsed() > std::time::Duration::from_secs(45) {
                let _ = client.kill();
                let _ = client.wait();
                p.forget_credentials();
                return Err(CallError::Failed(format!(
                    "the Remote Desktop session for {} did not start; check that the account may sign in over Remote Desktop",
                    account.user
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        let name = format!("rdp-session-{session}");
        Ok((
            Screen::Rdp(RdpScreen {
                account,
                session,
                width,
                height,
                client,
                dir,
                n,
                tasks: AtomicU32::new(0),
                pids: Vec::new(),
            }),
            name,
        ))
    }

    fn launch(
        &self,
        screen: &mut Screen,
        program: &str,
        args: &[String],
    ) -> Result<u32, CallError> {
        match screen {
            Screen::Hidden(d) => self.hidden.launch(d, program, args),
            Screen::Rdp(r) => {
                let out = r
                    .dir
                    .join(format!("pid-{}.txt", r.tasks.load(Ordering::Relaxed) + 1));
                r.run(
                    &crate::rdp::launch_script(program, args, &out.display().to_string()),
                    Some(&out),
                )?;
                let pid: u32 = std::fs::read_to_string(&out)
                    .map_err(CallError::failed)?
                    .trim()
                    .parse()
                    .map_err(|_| CallError::Failed(format!("{program} did not start")))?;
                let _ = std::fs::remove_file(&out);
                r.pids.push(pid);
                Ok(pid)
            }
        }
    }

    fn alive(&self, screen: &mut Screen) -> Vec<u32> {
        match screen {
            Screen::Hidden(d) => self.hidden.alive(d),
            Screen::Rdp(r) => {
                r.pids.retain(|p| process_running(*p));
                r.pids.clone()
            }
        }
    }

    fn capture(&self, screen: &Screen, path: &std::path::Path) -> Result<(), CallError> {
        match screen {
            Screen::Hidden(d) => self.hidden.capture(d, path),
            Screen::Rdp(r) => {
                let out = r
                    .dir
                    .join(format!("shot-{}.png", r.tasks.load(Ordering::Relaxed) + 1));
                r.run(
                    &crate::rdp::capture_script(r.width, r.height, &out.display().to_string()),
                    Some(&out),
                )?;
                std::fs::copy(&out, path).map_err(CallError::failed)?;
                let _ = std::fs::remove_file(&out);
                Ok(())
            }
        }
    }

    fn click(&self, screen: &Screen, x: i32, y: i32) -> Result<(), CallError> {
        match screen {
            Screen::Hidden(d) => self.hidden.click(d, x, y),
            Screen::Rdp(r) => r.run(&crate::rdp::click_script(x, y), None),
        }
    }

    fn type_text(&self, screen: &Screen, text: &str) -> Result<(), CallError> {
        match screen {
            Screen::Hidden(d) => self.hidden.type_text(d, text),
            Screen::Rdp(r) => r.run(
                &crate::rdp::keys_script(&crate::rdp::send_keys_text(text)),
                None,
            ),
        }
    }

    fn press_key(&self, screen: &Screen, chord: &Chord) -> Result<(), CallError> {
        match screen {
            Screen::Hidden(d) => self.hidden.press_key(d, chord),
            Screen::Rdp(r) => {
                let keys = crate::rdp::send_keys_chord(chord).ok_or_else(|| {
                    CallError::Failed(format!("{chord} has no SendKeys form (the Windows key)"))
                })?;
                r.run(&crate::rdp::keys_script(&keys), None)
            }
        }
    }

    fn stop(&self, screen: Screen) {
        match screen {
            Screen::Hidden(d) => self.hidden.stop(d),
            Screen::Rdp(mut r) => {
                let p = plans();
                p.logoff(r.session);
                let _ = r.client.kill();
                let _ = r.client.wait();
                p.forget_credentials();
                let _ = std::fs::remove_dir_all(&r.dir);
            }
        }
    }
}
