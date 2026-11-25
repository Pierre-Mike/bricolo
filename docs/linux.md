# Linux

The node runs on Linux with or without a desktop. This page lists what each desktop capability needs, what works on X11, on Hyprland, on other Wayland compositors and with no display, and how to run the Linux tests. The code is in `crates/providers/src/linux/`.

## What works where

| Capability | X11 | Hyprland | Other Wayland | No display |
|---|---|---|---|---|
| `clipboard` | Yes (X11 selections) | Yes (data-control) | Yes on compositors with a data-control protocol, such as Sway and KDE Plasma. Otherwise the X11 clipboard through XWayland, which the compositor keeps in sync | `unavailable` |
| `hotkey` | Yes (key grabs) | Yes (Hyprland binds) | `unavailable`, logged once | `unavailable`, logged once |
| `keyboard` | Yes (XTEST) | Yes: `keystroke` through Hyprland, `type-text` with `wtype` | `unavailable`, logged once | `unavailable`, logged once |
| `mic` | Yes, with ffmpeg (PulseAudio, else ALSA) | Yes, with ffmpeg | Yes, with ffmpeg | Yes, if a sound server or ALSA device is there |
| `window` | Yes, with an EWMH window manager | Yes (IPC socket) | `unavailable`, logged once | `unavailable`, logged once |
| `app.launch` | Yes | Yes | Yes | Yes, if the app can start without a display |
| `app.open-url` | Yes (`xdg-open`) | Yes (`xdg-open`) | Yes (`xdg-open`) | Runs `xdg-open`, which usually fails |
| `window.close`, `window.set-minimized` | Yes (`_NET_CLOSE_WINDOW`, `WM_CHANGE_STATE`; restoring activates) | Yes (minimizing parks the window on the `special:bricolo-minimized` workspace; restoring brings it to the active workspace and focuses it) | `unavailable` | `unavailable` |
| `app.quit`, `app.set-hidden` | Yes (closes, minimizes or restores every window of the app) | `unavailable` | `unavailable` | `unavailable` |
| `app.running`, `app.frontmost` | Yes (from the window list) | Yes (from the window list) | `unavailable` | `unavailable` |
| `screen` | Yes (`GetImage` on the root window) | Yes, with `grim` | Yes, with `grim` on compositors with wlr-screencopy | `unavailable`, logged once |
| `system.power` | Yes (`/sys/class/power_supply`) | Yes | Yes | Yes |
| `system.idle-seconds` | Yes (MIT-SCREEN-SAVER) | Through logind's idle hint | Through logind's idle hint | Through logind, else `unavailable` |
| `system.keep-awake`, `system.watch` | Yes (logind; screen changes through RandR) | Yes (logind), without screen changes | Yes (logind), without screen changes | Yes with logind, else `unavailable` |
| `secrets` | Yes (Secret Service) | Yes (Secret Service) | Yes (Secret Service) | `unavailable` without a session bus and a keyring |
| `notify` | Yes (D-Bus) | Yes (D-Bus) | Yes (D-Bus) | `unavailable` unless a session bus and a notification server run |
| `tts` | Yes | Yes | Yes | Yes |
| Everything else | Yes | Yes | Yes | Yes |

The node decides the session from the environment on each call: `WAYLAND_DISPLAY` set, or `XDG_SESSION_TYPE=wayland`, means Wayland, even when `DISPLAY` is also set for XWayland. Otherwise `DISPLAY` set means X11, and neither means no display. A Wayland session with `HYPRLAND_INSTANCE_SIGNATURE` set and Hyprland's socket in place is Hyprland. A node started before the desktop picks it up on the next call.

`bricolo run` on Linux runs the node on the calling thread. There is no main-thread loop as on macOS, so the desktop capabilities also work in tests and in `bricolo call`.

## X11

### Windows and screens

`window` reads the window manager's EWMH properties over x11rb, a pure Rust X11 client:

- `list` is `_NET_CLIENT_LIST`, without docks, panels, desktops, menus, splash screens and notifications (`_NET_WM_WINDOW_TYPE`). `app` is the class part of `WM_CLASS` ("XTerm", "firefox"). `title` is `_NET_WM_NAME`, or `WM_NAME`.
- `id` is the X11 window id. `focus` and `set-frame` refuse ids that are not in the client list.
- `frame` is the outer frame, decorations included: the client window's position on the root window, grown by `_NET_FRAME_EXTENTS`. This matches the macOS backend, which reports the whole window.
- `focused` is `_NET_ACTIVE_WINDOW`.
- `focus` sends a `_NET_ACTIVE_WINDOW` request as a pager would, so the window manager switches desktop, raises the window and focuses it. Without EWMH support it raises the window and sets the input focus itself.
- `set-frame` first takes the window out of the maximized state, then sends `_NET_MOVERESIZE_WINDOW` with north-west gravity, so the outer frame's corner lands on `x` and `y`. Terminals and some editors size in whole character cells, so the size can come out a few pixels smaller than asked. Without `_NET_MOVERESIZE_WINDOW`, it configures the window directly.
- `screens` lists the RandR 1.5 monitors. `visible-frame` is the monitor cut down to `_NET_WORKAREA`, the area free of panels. The work area spans all monitors, so a panel on one monitor only shrinks that monitor. `main` is the RandR primary monitor, else the first. A server without RandR gives one screen, the root window.

Coordinates are pixels from the top-left corner of the root window. On macOS they are points; on a HiDPI X11 desktop, one point is not one pixel.

Without a window manager, or with one that does not set `_NET_CLIENT_LIST`, the list is empty. Openbox, Xfwm, KWin, Mutter on X11, i3 and awesome all set it.

### Hotkeys

`hotkey` grabs each chord on the root window with `XGrabKey`, on a connection of its own, and a thread named `bricolo-hotkeys` delivers the presses. The modifiers map to the usual X11 layout: `cmd` is Super (Mod4), `alt` is Mod1, `ctrl` and `shift` are themselves. Each chord is grabbed four times, with and without Caps Lock and Num Lock, so the locks do not block it.

- Keys are looked up in the server's keyboard map, so a chord follows the layout: `cmd+alt+q` grabs the key that types `q`.
- A chord another X11 program already grabbed fails with `cannot bind <chord>: another program holds it`. Desktops grab some chords for themselves, such as Super+Left on GNOME.
- Holding a chord repeats it with the keyboard's auto-repeat.
- If the X server goes away, the next `bind` connects again and grabs every binding again.

### Keyboard

`keyboard` sends fake key presses through the XTEST extension, which Xorg and Xvfb have, on a connection opened for each call.

- `keystroke` looks the key up in the server's keyboard map, as hotkeys do, and holds Shift_L, Control_L, Alt_L or Super_L for the modifiers.
- `type-text` types one character at a time. A character in the first column of the map is its key; one in the second column gets Shift. A character the map lacks (most of Unicode on a US map) goes through an unused keycode, which the node maps to that character first, as xdotool does. Those keycodes keep their new mapping afterwards.
- Modifiers you still hold when the call starts, such as the chord that triggered it, are released first, so they don't change what gets typed.
- On Wayland there is no protocol for one client to type into another, so `keyboard` answers `unavailable`.

### Microphone

`mic` records with ffmpeg: `-f pulse -i default` first, which PipeWire also serves through `pipewire-pulse`, then `-f alsa -i default`. `[mic] device` in node.toml replaces `default`. ffmpeg needs no display, so `mic` works on Wayland and headless as long as a sound server or an ALSA device is there.

### Apps

- `launch` takes a desktop file id (`firefox`, `org.gnome.Nautilus`, with or without `.desktop`) or an app's `Name` ("Files"), `StartupWMClass` or program name, in that order, any case. It looks in `$XDG_DATA_HOME/applications` (default `~/.local/share/applications`) and in each `$XDG_DATA_DIRS` entry (default `/usr/local/share:/usr/share`), with ids from subfolders as the spec says (`kde/konsole.desktop` is `kde-konsole.desktop`). Entries with `Hidden=true` hide the same id in later folders.
- If a window of the app is open (its `WM_CLASS` matches the name asked for, the entry's `StartupWMClass`, its program or its `Name`), `launch` raises that window instead, as `open -a` does on macOS.
- Otherwise it runs `gio launch <file>`, or `gtk-launch <id>`, or, with neither installed, the entry's `Exec` line itself with the field codes (`%U` and the rest) removed. With no desktop file, it runs a program of that name from `PATH`. Paths are refused, as on macOS.
- `open-url` runs `xdg-open`. It refuses `file:` URLs with `denied`, as on macOS. Without `xdg-open` it answers `unavailable`.
- `running` lists one entry per app window owner: the `WM_CLASS` class as `name` and `_NET_WM_PID` as `pid` (0 when the window does not set it). `bundle-id` is always empty on Linux. `frontmost` is the owner of the active window.
- `launch` and `open-url` wait up to 10 seconds for the launcher to hand over, then leave it running.
- An app's windows are those whose `WM_CLASS` instance or class matches the name, or a window class its desktop file gives, so "Files" finds Nautilus. `quit` sends `_NET_CLOSE_WINDOW` to each, as their close buttons do, so the app can save or ask first; it never kills the process. `set-hidden` minimizes them all, or restores them all and leaves the newest in front. Both answer `failed` with "no such app" when no window matches, as on macOS.

### Screen

`screen.capture` reads the root window with `GetImage` on a connection opened for each call, converts the pixels to RGBA from the visual's masks and the server's byte order, and writes a PNG to `<data dir>/<plugin>/screenshots/`, as on macOS. Visuals other than TrueColor or DirectColor at 24 or 32 bits answer `failed`.

- With no area it captures the whole root window, every monitor. macOS captures only the main screen.
- An area partly off screen is clipped to the root window; one wholly off screen answers `failed`.
- On Wayland it runs `grim` (`grim -g "X,Y WxH"`), and answers `unavailable` without it.

## System

- `power` reads `/sys/class/power_supply`. The battery is the mean `capacity` of the system's batteries, leaving out peripherals (`scope=Device`) and absent ones. Plugged in means a Mains or USB supply is `online`, or, with none, that no battery is `Discharging`. A desktop with no supplies reads `battery: none`, plugged in.
- `idle-seconds` asks the X server's MIT-SCREEN-SAVER extension on X11. Elsewhere it reads logind's `IdleHint` and `IdleSinceHint` for the session, which stay 0 until the desktop marks the session idle, so it is coarser than on macOS.
- `keep-awake` takes a logind inhibitor lock (`idle:sleep`, mode `block`) on the system bus and holds it until `keep-awake(false)` or the plugin stops. Unlike `caffeinate -i` on macOS, it also blocks a suspend the user asks for. Without permission from polkit it answers `denied`.
- `watch` sends `will-sleep` and `did-wake` from logind's `PrepareForSleep`, `screen-locked` and `screen-unlocked` from the session's `Lock`, `Unlock` and `LockedHint`, and `screens-changed` from RandR on X11.
- Without a system bus or logind, as in most containers, the logind parts answer `unavailable` at once. Every D-Bus call gives up after 5 seconds.

## Secrets

`secrets` keeps each value as an item in the Secret Service's default collection on the session bus, which GNOME Keyring, KWallet 5.97 or later and KeePassXC serve. Items carry the attributes `service = bricolo` and `account = <plugin>/<name>`, and the label `bricolo: <plugin>/<name>`, so `secret-tool search service bricolo` lists them.

- The node looks for the session bus and the service on each call, so a keyring that starts after the node is found.
- A locked keyring is unlocked when that needs no password. If it needs one, the call answers `failed` and asks you to unlock it first. The node never creates a keyring.
- Each call gives up after 10 seconds.

## Hyprland

On Hyprland, `window`, `hotkey`, `keyboard`, `app.running` and `app.frontmost` go through Hyprland's IPC sockets in `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/` (`/tmp/hypr/...` before 0.40). The code is in `crates/providers/src/linux/hyprland/`, and `linux/desktop.rs` picks Hyprland or X11 on each call.

Hyprland 0.55 and later read a Lua config, and take actions as `eval` with `hl.*` calls; `keyword` and the old `dispatch` lines are refused. Older releases, and newer ones still on a `hyprland.conf`, take `dispatch` and `keyword` lines. The node sends `eval return 1` once per Hyprland instance: `ok` means Lua, anything else the legacy commands. Queries (`j/clients`, `j/monitors`, `j/activewindow`, `j/binds`) are the same in both. We tested on Hyprland 0.56 with Omarchy's Lua config; the legacy commands are covered by unit tests only.

### Windows and screens

- `list` is `j/clients`: the mapped windows that are not hidden members of a group, on every workspace, special ones included. `id` is the window's address as a number (`0x55fe1313a980`), `app` its class.
- `frame` is Hyprland's `at` and `size`, in layout coordinates: pixels divided by the monitor's scale, from the top-left corner of the layout.
- `focus` focuses the window, switching workspace if needed.
- `set-frame` floats a tiled window first (the tiling layout owns a tiled window's frame), then sets its size and its position. It refuses a fullscreen window.
- `screens` is `j/monitors`, scaled into layout coordinates and turned for rotated monitors. `visible-frame` leaves out the monitor's reserved area (bars such as Waybar). Hyprland has no primary monitor, so `main` is the focused one.

### Hotkeys

- Each chord becomes a Hyprland bind whose action emits `custom>>bricolo-hotkey:<token>` on the event socket (`.socket2.sock`), where a thread named `bricolo-hypr-events` picks it up. `cmd` is SUPER.
- A chord Hyprland's config already binds fails with `cannot bind <chord>: Hyprland's config already binds it`, as a taken X11 grab does. Binds written by keycode (Omarchy binds workspaces as `SUPER + code:10`) count: the node maps chords to keycodes of a standard (evdev, pc105) keyboard, and reads `code:` binds from the plain-text `binds` listing, since Hyprland 0.56's JSON leaves their keycode out.
- A config reload (saving the config, `hyprctl reload`) drops runtime binds, so the node binds every chord again on `configreloaded`.
- Lua binds carry the description `bricolo[<pid>]: <plugin>/<id>`, which shows in `hyprctl binds`; legacy binds carry the pid in the event. A node that was killed leaves its binds behind; the next node removes those of nodes no longer running when it binds its first chord.
- Binds from a virtual keyboard (`wtype`, `xdotool` through XWayland) do not fire Hyprland binds, so testing a chord needs the real keyboard.

### Keyboard

- `keystroke` asks Hyprland to send the chord to the focused window: `send_key_state` down, then up 50 ms later, as Omarchy's clipboard binds do (legacy: `sendshortcut`). Keys the user still holds do not mix in.
- After a `wtype` run, Hyprland reads key names in `wtype`'s keymap, which holds only the keys it just typed, and the focused app may still read keys with that keymap. `send_key_state` then fails with "key not found", or arrives as the wrong key. So after the node's own `type-text`, and whenever Hyprland answers "key not found", `keystroke` runs `wtype -M <mods> -k <key>` instead, after the same wait for released modifiers. The next hotkey press switches back to `send_key_state`. Through `wtype`, Shift reaches the app as a modifier but does not change the character (Shift+1 types `1`, not `!`). That's fine for shortcuts; use `type-text` for characters.
- `type-text` runs `wtype`, which types through the virtual keyboard protocol. Hyprland merges a virtual keyboard's keys with the modifiers held on the real keyboard, so with the Lua config the node first waits, up to 2 seconds, until no Super, Alt, Control or Shift key is down (`hl.is_key_down`). After the chord that triggered the call, typing starts when the user lets go. With the legacy commands there is no such check. Without `wtype`, `type-text` answers `unavailable`.

## Other Wayland compositors

Wayland has no protocol for one client to list, move or focus other clients' windows, or to grab keys. GNOME and KDE offer their own D-Bus interfaces and extensions for that; this project does not use them. So on Wayland compositors other than Hyprland `window`, `hotkey`, `app.running` and `app.frontmost` answer `unavailable`, and the node logs why the first time each is called.

- The XDG GlobalShortcuts portal would give hotkeys where the desktop's portal backend has it (KDE, Hyprland, recent GNOME), through `ashpd`. It is not wired in yet: the user confirms shortcuts in the desktop's own dialog, and we could not test that in the container.
- XWayland would let the X11 code see and move X11 apps only, and key grabs there fire only while an X11 app has focus. The node treats a Wayland session as Wayland, even with `DISPLAY` set.
- `clipboard` uses a data-control protocol (`wlr-data-control` or `ext-data-control`). On a compositor without one, arboard falls back to the X11 clipboard through XWayland, and answers `unavailable` without XWayland. We tested the clipboard on X11 and on Hyprland.

## No display

On a server, in a container or over SSH, the node runs headless: `log`, `timer`, `kv`, `disk`, `shell`, `http`, `tts`, `speech`, the sidecars, the Lua scripts and the mesh all work. `system.power` works too. `window`, `hotkey`, `keyboard`, `clipboard`, `screen`, `app.running` and `app.frontmost` answer `unavailable`, and so do `secrets` and most of `system` without a session bus, a keyring and logind. A plugin such as `window-tiler` that binds hotkeys in `init` goes `off` with that error. `notify` answers `unavailable` without a session bus or a notification server.

## TTS

With a TTS server in `node.toml`, the queue works as on macOS. The default player is the first of `pw-play` (PipeWire), `paplay` (PulseAudio, which PipeWire also serves) and `aplay` (ALSA) found on `PATH`; `[tts] player` overrides it.

With no server, the node speaks with the first of `espeak-ng`, `espeak` and `spd-say` on `PATH`, with the same queue: one chunk at a time, `stop` clears the queue and kills the speaker mid-chunk. `espeak-ng` and `espeak` get the text on stdin and the voice as `-v`, like `say`. `spd-say` gets `-w -y <voice> -- <text>`, and `stop` also runs `spd-say -C`, since killing `spd-say` leaves Speech Dispatcher talking. With none of them installed, `tts` answers `unavailable`.

## Packages

Building needs a C compiler (for the vendored Lua and `ring`) and nothing else: the X11, Wayland and D-Bus clients are pure Rust, with no `libX11`, `libwayland` or `libdbus` to link.

At run time, install what you use:

| For | Debian and Ubuntu packages |
|---|---|
| `app.launch` | `libglib2.0-bin` (`gio`), or `gtk-launch` from `libgtk-3-bin` |
| `app.open-url` | `xdg-utils` |
| `notify` | a notification server; desktops ship one, or `dunst` |
| `tts` without a server | `espeak-ng`, or `speech-dispatcher` |
| `tts` with a server | `pipewire-bin`, `pulseaudio-utils` or `alsa-utils` |
| `keyboard.type-text` on Hyprland, and `keystroke` after it | `wtype` |
| `mic` | `ffmpeg` |
| `screen` on Wayland | `grim` |
| `secrets` | a Secret Service: `gnome-keyring`, KWallet or KeePassXC |
| `system.keep-awake`, `system.watch` | systemd-logind, which systemd desktops run |

## Permissions

Linux asks for no permission like macOS Accessibility or Microphone. Any X11 client can read the window list, move windows, grab keys and type, so the node can too. The plugin permissions still apply: a plugin needs the `window`, `hotkey`, `keyboard`, `mic` or `app` grant you approve at install.

## Tests

`just linux-test` runs everything on Linux in Docker, with an arm64 or x86_64 Debian image matching the host (`scripts/linux-test/Dockerfile`). The checkout is mounted at `/src`; the Linux build goes to the Docker volume `bricolo-linux-target`, mounted over `/src/target`, and the cargo registry to `bricolo-linux-registry`. With several git worktrees, give each its own build volume, such as `BRICOLO_LINUX_TARGET=bricolo-linux-target-voice just linux-test`. Cargo decides what to rebuild from file times, so in a shared volume a build from another worktree's newer sources counts as fresh, and the tests load its plugins.

If the linker gets killed (`ld terminated with signal 9`), the Docker VM ran out of memory linking several test binaries at once. `CARGO_BUILD_JOBS=2 just linux-test` passes a lower job count into the container. With colima's 8 GiB VM, the full run needed it.

If the build fails with `No space left on device`, the Docker VM's disk is full. Each build volume holds a whole debug build, 15 to 35 GB. `docker system df -v` lists them; remove the volume of a worktree you no longer use, such as `docker volume rm bricolo-linux-target-voice`.

1. With no display: `cargo clippy` and `cargo nextest run --workspace`. This checks that everything works headless, and that the desktop capabilities answer `unavailable`. Every step leaves out `bricolo-ui`, since the image has no GTK or WebKitGTK; the Ubuntu `ci` job builds and tests it ([ui.md](ui.md)).
2. Inside `dbus-run-session`, on Xvfb at 1280x800 with openbox, dunst and an unlocked gnome-keyring in a temporary folder: the X11 tests, one at a time (`--test-threads 1`), since they share the screen, the keyboard and the clipboard. They are the tests named `x11_*` and `clipboard_round_trip`, all `#[ignore]` so a plain run skips them. They open xterms, press keys with `xdotool`, read the clipboard with `xclip`, and find the notification in dunst's history over D-Bus.
3. A smoke test (`scripts/linux-test/smoke.sh`): installs `window-tiler` and `fixtures/caps`, adds a Lua script that uses the clipboard, windows, notifications and a hotkey, starts `bricolo run`, drives it with `bricolo fire` and `xdotool`, and stops it with SIGTERM.

`just linux-test x11` and `just linux-test smoke` run one part; `just linux-test shell` opens a shell on that desktop. CI runs the same three steps on `ubuntu-latest`.

`just check-linux` still cross-checks from macOS with zig. The new dependencies are pure Rust, so it needs nothing more.
