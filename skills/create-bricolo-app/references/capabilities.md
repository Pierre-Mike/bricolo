# Capability cheat-sheet

Generated from `wit/*.wit` and the `bricolo` table in docs/scripts.md by `just skill-refs`. Do not edit it by hand. `bricolo wit <name>` prints the same WIT from the installed `bricolo`.

- **Plugins** import each capability they call: list it in `imports` in plugin.toml, and `bricolo check` refuses a component that imports one not listed. In Rust an interface is `bricolo::stdlib::<name>` (dashes become underscores). An `async func` is awaited. Async functions take owned `String`s; sync ones take `&str`.
- **Scripts** call `bricolo.<name>.<call>`. Calls that wait return their value, or `nil, err` on failure. Scripts get every capability without a prompt.
- Every failure is a `call-error`: `unavailable`, `timeout`, `denied` or `failed(msg)`. In Lua, `err` is `"unavailable"`, `"timeout"`, `"denied"` or `"failed: <msg>"`.

## What you want, and what does it

| You want to | Use |
|---|---|
| Run something on a key chord | `hotkey` |
| Run something every N seconds, or once later | `timer` |
| Read or write the clipboard text | `clipboard` (no change event: poll with `timer`) |
| Find, focus, move or resize windows; screen sizes | `window` |
| Launch apps, list running ones, the app in front | `app` |
| Show a system notification | `notify` |
| Show a toast, a caption, a pick list, an edit box, controls (switches and buttons), a widget with values worth a glance | `ui` (drawn by the hub, `bricolo-ui`) |
| Press keys or type text into the app in front | `keyboard` |
| Move the pointer, click, scroll, read where the pointer is | `mouse` |
| Record the microphone to a WAV file | `mic` |
| Play a sound file, once or looped, and change its volume | `audio` |
| Speech to text | `speech` (needs a `[speech]` server, often a sidecar) |
| Text to speech | `tts` (`say` or espeak-ng, or a `[tts]` sidecar) |
| Read and write files | `disk` (plugins need a `disk:` grant per folder) |
| Remember small values across restarts | `kv` |
| Call a web API or a local server, a sidecar included | `http` (plugins need `[http] allow`) |
| Run a command-line program | `shell` (plugins need `[shell] allow` with its absolute path) |
| Write to the log | `log` |
| OCR, an LLM, embeddings, image or audio processing, a database, a Python or Node.js package, the GPU | No capability: a sidecar, called through `http` |
| Take a screenshot | No capability: `shell` with `screencapture` (macOS) or `maim` (Linux), or a sidecar |
| Read the selected text in another app | No capability: `keyboard` cmd+c, then `clipboard` |
| Read screen pixels, read another app's UI elements | Missing. Tell the user |

## Interfaces

| Interface | What it does |
|---|---|
| [`accessibility`](#accessibility) | The accessibility tree of an app's windows: read it, find controls, and click, type into, pick menu items in, or act on them, in the background where the system allows. Needs the Accessibility permission on macOS (screenshots also need Screen Recording); uses UI Automation on Windows and AT-SPI on Linux. Always runs on this node. Prefer shell, files and http when they can do the job: they are faster and leave the user's screen alone. Never submit, send, buy or delete through an app unless the user asked for that.  `app` selects an app by bundle id, name or `pid:<number>`, as `list-apps` reports them. Element indexes come from the last `snapshot` of that app: they stay the same while the element exists, and go stale when it goes away. Every action returns the window's fresh state. |
| [`app`](#app) | Apps on this device. |
| [`audio`](#audio) | Plays sound files on this device's default output. Always runs on this node. A plugin may play several sounds at once and controls only its own; they stop when it stops or reloads. |
| [`camera`](#camera) | Photos from this device's camera, taken with ffmpeg like `mic`. Needs the Camera permission on macOS; without it calls answer `failed`. Always runs on this node. |
| [`claude-code`](#claude-code) | Claude Code sessions on this node: start one on a task in a folder, follow its state, send it more work, stop it. The node runs the `claude` CLI headless. Folders must be under `[claude-code] dirs` in node.toml; without that section every call answers `unavailable`. |
| [`clipboard`](#clipboard) | The system clipboard: text, images and copied files. |
| [`device`](#device) | The volume, brightness, Wi-Fi, appearance, Focus, lock and sleep of this device. Always runs on this node. A setting the system has no way to change here answers `unavailable`. |
| [`disk`](#disk) | Files outside the plugin's own data folder, limited to the paths granted at install (`disk:~/Notes`, `disk:read:~/Documents`). Paths are absolute or start with `~/`. |
| [`files`](#files) | Watches folders for changes. A plugin may watch only paths under its `disk:` grants. Always runs on this node. |
| [`hotkey`](#hotkey) | Global keyboard shortcuts. |
| [`http`](#http) | HTTP requests to the `host:port` pairs listed under `[http] allow` in plugin.toml. Others answer `denied`. Redirects are not followed: a 3xx comes back as the response. |
| [`keyboard`](#keyboard) | Synthetic key presses on this device: shortcuts and typed text. Always runs on this node, never on a peer. On macOS it needs the Accessibility permission (else `denied`); on Linux an X11 session with the XTEST extension (on Wayland and without a display, `unavailable`). On Windows it answers `denied` when the focused app runs as administrator. |
| [`kv`](#kv) | A small key/value store private to each plugin, kept in its data folder. It survives restarts and reloads. |
| [`log`](#log) | Plugin logging. Lines go to the node's daily log file. |
| [`mic`](#mic) | Records the microphone of this device into the caller's data folder, through ffmpeg. Always runs on this node. One recording at a time per plugin; `bricolo status` lists who is recording under `active_sensors`. |
| [`mouse`](#mouse) | The mouse pointer of this device. Points use the same coordinates as `window`. Moving and clicking need the Accessibility permission on macOS; without it they answer `denied`. Always runs on this node. |
| [`notify`](#notify) | Notifications shown to the user. |
| [`process`](#process) | Programs running on this device, with their CPU and memory use, and stopping or killing one. Always runs on this node. The node refuses to stop itself, its parent and the system's first process. |
| [`screen`](#screen) | Pictures of this device's screens. Needs the Screen Recording permission on macOS; without it calls answer `denied`. Always runs on this node. |
| [`scripting`](#scripting) | Runs AppleScript, JavaScript for Automation and Shortcuts on macOS, and PowerShell on Windows (and wherever `pwsh` is installed). Always runs on this node. A script can do anything the user can, so it is as trusted as `shell`. On macOS each app a script drives asks the user once for Automation access. A script that runs longer than a minute is stopped and answers `timeout`. |
| [`secrets`](#secrets) | Small secrets such as API keys, kept in the macOS Keychain, a Linux Secret Service or the Windows Credential Manager. Each plugin sees only its own. A name is 1 to 128 letters, digits, `.`, `_` or `-`; any other name answers `failed`. Always runs on this node. |
| [`selection`](#selection) | The text selected in the frontmost app. Needs the Accessibility permission on macOS; without it calls answer `denied`. Always runs on this node. |
| [`shell`](#shell) | Runs programs listed under `[shell] allow` in plugin.toml. No shell is involved: `program` is an exact absolute path from that list, started with `args`. The program gets HOME, a fixed PATH, and `env`, whose names must be listed under `[shell] env`. Anything else answers `denied`. A program that runs past 60 seconds is killed and the call answers `timeout`. |
| [`speech`](#speech) | Speech to text. |
| [`system`](#system) | The state of this device. Always runs on this node. |
| [`timer`](#timer) | Timers. `after` and `every` return an id at once; `timer(id)` events follow. |
| [`tts`](#tts) | Text to speech. Speech is queued: `speak` returns once the text is queued. The node splits long text into sentence chunks and speaks them one at a time. |
| [`ui`](#ui) | A tile of controls in the plugin's hub card, a display-only widget, and a few kinds of floating panel, described as data. The UI process draws them; plugins never ship HTML or JavaScript. What the user does arrives as a `ui` event. Always runs on this node. |
| [`virtual-screen`](#virtual-screen) | Screens that no monitor shows, where apps run and are driven without touching the user's screen, keyboard or pointer. Linux runs each on Xvfb, and `accessibility` reaches the apps on it too. Windows Server runs each in a Remote Desktop session of its own, once an account for it is stored as secrets; other Windows editions use a hidden desktop, pictured window by window and driven by window messages, where key chords with modifiers are refused. macOS answers `unavailable` for now. Always runs on this node. |
| [`window`](#window) | Windows and screens of this device. Points are measured from the top-left corner of the main display, y growing downward. Needs the Accessibility permission on macOS; without it calls answer `denied`. |

## Shared types

Types shared by every interface.

```wit
package bricolo:stdlib@0.1.0;

/// Types shared by every interface.
interface types {
  /// Why a capability call failed. Every call that can run on another
  /// device returns this, so a plugin always handles the remote case.
  variant call-error {
    /// No node offering this capability is online.
    unavailable,
    /// The deadline passed.
    timeout,
    /// A permission or peer trust refused the call.
    denied,
    /// The provider ran and reported an error.
    failed(string),
  }

  /// A tile switch the user toggled, with its new value.
  record switch-toggled {
    id: string,
    on: bool,
  }

  /// The entry picked in a choice list, or none when it was dismissed.
  record choice-picked {
    panel: string,
    index: option<u32>,
  }

  /// A review panel's answer, with the text as the user left it.
  record review-done {
    panel: string,
    accepted: bool,
    text: string,
  }

  /// What the user did with this plugin's tile or panels (`ui`).
  variant ui-event {
    switch(switch-toggled),
    /// A tile button was pressed. Carries its id.
    button(string),
    choice(choice-picked),
    review(review-done),
  }

  /// What happened to an app (`app.watch`).
  enum app-change {
    launched,
    terminated,
    /// It came to the front.
    activated,
  }

  record app-event {
    change: app-change,
    name: string,
    bundle-id: option<string>,
    pid: s32,
  }

  /// What happened to the device (`system.watch`).
  enum system-event {
    will-sleep,
    did-wake,
    screen-locked,
    screen-unlocked,
    /// A display was added or removed, or its arrangement changed.
    screens-changed,
  }

  /// What changed under a watched folder (`files.watch`).
  enum file-change {
    created,
    modified,
    removed,
  }

  record file-event {
    /// The id `files.watch` returned.
    watch: u64,
    path: string,
    change: file-change,
  }

  /// What the node delivers to `on-event`. Later versions add cases, so
  /// match with a fallback arm.
  variant event {
    /// A timer from `timer.after` or `timer.every` fired. Carries its id.
    timer(u64),
    /// A hotkey bound with `hotkey.bind` was pressed. Carries the binding id.
    hotkey(string),
    /// A chord bound with `hotkey.bind` was released. Carries the binding
    /// id. A node that doesn't see releases never sends it.
    hotkey-released(string),
    /// The user acted on this plugin's tile or panel.
    ui(ui-event),
    /// An app launched, quit or came to the front, after `app.watch(true)`.
    app(app-event),
    /// The device slept, woke, locked or unlocked, or its screens changed,
    /// after `system.watch(true)`.
    system(system-event),
    /// Something changed under a folder watched with `files.watch`.
    file(file-event),
  }
}
```

## accessibility

The accessibility tree of an app's windows: read it, find controls, and click, type into, pick menu items in, or act on them, in the background where the system allows. Needs the Accessibility permission on macOS (screenshots also need Screen Recording); uses UI Automation on Windows and AT-SPI on Linux. Always runs on this node. Prefer shell, files and http when they can do the job: they are faster and leave the user's screen alone. Never submit, send, buy or delete through an app unless the user asked for that.  `app` selects an app by bundle id, name or `pid:<number>`, as `list-apps` reports them. Element indexes come from the last `snapshot` of that app: they stay the same while the element exists, and go stale when it goes away. Every action returns the window's fresh state.

macOS (needs the Accessibility permission; screenshots need Screen Recording), Windows (UI Automation) and Linux (AT-SPI). Prefer shell, disk and http; take a `snapshot`, act on its indexes, read each result's `verification`, and never submit, send, buy or delete unless asked. See docs/computer-use.md. Never routed; a peer or the MCP gateway calls it by node.

```wit
/// The accessibility tree of an app's windows: read it, find controls, and
/// click, type into, pick menu items in, or act on them, in the background
/// where the system allows. Needs the Accessibility permission on macOS
/// (screenshots also need Screen Recording); uses UI Automation on Windows
/// and AT-SPI on Linux. Always runs on this node. Prefer shell, files and
/// http when they can do the job: they are faster and leave the user's
/// screen alone. Never submit, send, buy or delete through an app unless
/// the user asked for that.
///
/// `app` selects an app by bundle id, name or `pid:<number>`, as
/// `list-apps` reports them. Element indexes come from the last `snapshot`
/// of that app: they stay the same while the element exists, and go stale
/// when it goes away. Every action returns the window's fresh state.
interface accessibility {
  use types.{call-error};
  use window.{rect};

  record ax-app {
    name: string,
    bundle-id: option<string>,
    pid: s32,
    /// The app the user is working in.
    frontmost: bool,
  }

  record ax-window {
    /// Pass it as `window` to `snapshot`.
    index: u32,
    title: string,
    frame: option<rect>,
    focused: bool,
    minimized: bool,
  }

  /// One read of a window's tree.
  record app-state {
    app: string,
    pid: s32,
    /// The window's title.
    window: string,
    /// One line per element, indented by depth: its index, role, traits,
    /// name, value and extra actions. Act on the leading index.
    tree: string,
    /// How many elements were read. Indexes may be sparse: never infer one.
    element-count: u32,
    /// The read stopped at the element or depth limit; lines saying
    /// `... N more not shown` mark where. Snapshot that element to see them.
    truncated: bool,
    /// The element with keyboard focus, if any.
    focused: option<u32>,
    /// The PNG of the window, when asked for.
    screenshot: option<string>,
  }

  record element {
    index: u32,
    role: string,
    name: option<string>,
    value: option<string>,
    frame: option<rect>,
    enabled: bool,
    focused: bool,
    /// The actions `perform` takes for this element.
    actions: list<string>,
  }

  /// How sure the node is that an action did what was asked.
  enum verification {
    /// The new value or text was read back.
    verified,
    /// The system took the action; nothing could be read back.
    unverified,
    /// The system took the action, but nothing in the window changed
    /// within a second. Web and Electron controls often ignore it.
    suspected-noop,
    /// Text was read back and differs from what was sent: check before
    /// typing again.
    mismatch,
  }

  record action-result {
    verification: verification,
    /// What was checked, or why nothing could be.
    detail: option<string>,
    /// The window after the action.
    state: app-state,
  }

  /// Apps with a window, the frontmost first.
  list-apps: async func() -> result<list<ax-app>, call-error>;
  /// The app's windows, dialogs included.
  list-windows: async func(app: string) -> result<list<ax-window>, call-error>;
  /// Reads a window's tree: the focused window, or `window` from
  /// `list-windows`. `query` prints only elements whose line contains the
  /// text (any case), with their ancestors. `element` reads only that
  /// element and what is under it. `screenshot` also saves a PNG of the
  /// window. Indexes are the same whatever is printed.
  snapshot: async func(app: string, window: option<u32>, query: option<string>, element: option<u32>, screenshot: option<bool>) -> result<app-state, call-error>;
  /// The elements whose role, name or value contains `text`, any case.
  find: async func(app: string, text: string) -> result<list<element>, call-error>;
  /// Clicks an element through its accessibility action, without moving
  /// the pointer.
  click: async func(app: string, element: u32) -> result<action-result, call-error>;
  /// Sets a field's value, a checkbox, a slider or a popup's choice, and
  /// reads it back.
  set-value: async func(app: string, element: u32, value: string) -> result<action-result, call-error>;
  /// Inserts text at the caret of `element`, or of the focused field when
  /// none, and reads it back.
  type-text: async func(app: string, element: option<u32>, text: string) -> result<action-result, call-error>;
  /// Presses a key or chord, like `return`, `escape`, `tab` or `cmd+a`,
  /// in the app. Chords that quit, close, log out or lock are refused: use
  /// the app's own button or menu item.
  press-key: async func(app: string, key: string) -> result<action-result, call-error>;
  /// Runs one of the element's listed `actions`.
  perform: async func(app: string, element: u32, action: string) -> result<action-result, call-error>;
  /// Picks a menu bar item by its path, like `["File", "Export…"]`. Names
  /// match any case, with or without a trailing ellipsis.
  menu: async func(app: string, path: list<string>) -> result<action-result, call-error>;
  /// Waits until an element's line contains `text`, for up to
  /// `timeout-ms` (10 seconds when none), and returns those elements.
  /// Fails with `timeout` when it never shows.
  wait: async func(app: string, text: string, timeout-ms: option<u32>) -> result<list<element>, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.accessibility.list_apps()` | list of `{name, bundle_id, pid, frontmost}` | Waits. Apps with a window. docs/computer-use.md has the rules agents follow. |
| `bricolo.accessibility.list_windows(app)` | list of `{index, title, frame, focused, minimized}` | Waits. `app` is a bundle id, a name or `"pid:123"`. |
| `bricolo.accessibility.snapshot(app, window, query, element, screenshot)` | `{app, pid, window, tree, element_count, truncated, focused, screenshot}` | Waits. Every argument after `app` may be `nil`. `tree` has one line per element, starting with its index. |
| `bricolo.accessibility.find(app, text)` | list of `{index, role, name, value, frame, enabled, focused, actions}` | Waits. |
| `bricolo.accessibility.click(app, index)` | `{verification, detail, state}` | Waits. `verification` is `"verified"`, `"unverified"`, `"suspected-noop"` or `"mismatch"`; `state` is the fresh snapshot. |
| `bricolo.accessibility.set_value(app, index, value)` | result as `click` | Waits. Reads the value back. |
| `bricolo.accessibility.type_text(app, index, text)` | result as `click` | Waits. `index` may be `nil` for the focused field. |
| `bricolo.accessibility.press_key(app, key)` | result as `click` | Waits. `"return"`, `"cmd+a"`; quit, close and lock chords answer `"failed: system_shortcut_blocked…"`. |
| `bricolo.accessibility.perform(app, index, action)` | result as `click` | Waits. One of the element's `actions`. |
| `bricolo.accessibility.menu(app, path)` | result as `click` | Waits. `{"File", "Export…"}`. |
| `bricolo.accessibility.wait(app, text, timeout_ms)` | list of elements | Waits until an element shows `text`; `nil, "timeout"` after `timeout_ms` (10 s when `nil`). |

## app

Apps on this device.

macOS, Linux (desktop files; `running`, `frontmost`, `quit` and `set-hidden` need X11), and Windows (`.exe` names). `watch` works on macOS and Windows.

```wit
/// Apps on this device.
interface app {
  use types.{call-error};

  record app-info {
    name: string,
    bundle-id: option<string>,
    pid: s32,
  }

  /// Launches an app, or brings it to the front if it runs. `app` is a
  /// bundle id ("com.apple.Safari") or a name ("Safari"), not a path.
  launch: async func(app: string) -> result<_, call-error>;
  /// Opens `url` with its default app. `file:` URLs answer `denied`.
  open-url: async func(url: string) -> result<_, call-error>;
  /// Apps that show in the Dock.
  running: async func() -> result<list<app-info>, call-error>;
  frontmost: async func() -> result<option<app-info>, call-error>;
  /// Starts or stops delivering `app` events to this plugin: launches,
  /// quits, and the app that comes to the front.
  watch: async func(on: bool) -> result<_, call-error>;
  /// Asks the app to quit, as Cmd+Q does. `app` is a bundle id or a name.
  /// Answers `failed` when no such app runs.
  quit: async func(app: string) -> result<_, call-error>;
  /// Hides the app's windows, as Cmd+H does, or shows them again when
  /// `hidden` is false. Answers `failed` when no such app runs.
  set-hidden: async func(app: string, hidden: bool) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.app.launch(app)` | `true` | Waits. `app` is a bundle id like `"com.apple.Notes"` or an app name like `"Notes"`, not a path. On Linux, a desktop file id like `"firefox"` or an app name like `"Files"`. On Windows, a program name like `"notepad"`, found through the Start Menu, App Paths or `PATH`. |
| `bricolo.app.open_url(url)` | `true` | Waits. `file:` URLs answer `"denied"`. |
| `bricolo.app.running()` | list of apps | Waits. An app is `{name, bundle_id, pid}`. Only apps that show in the Dock; on Linux and Windows, apps with a window (`bundle_id` is `nil`). |
| `bricolo.app.frontmost()` | app or `nil` | Waits. |
| `bricolo.app.quit(app)` | `true` | Waits. Asks the app to quit, as Cmd+Q does. `app` is a bundle id or a name; `nil, "failed: no such app"` when none runs. On Linux (X11 only) and Windows it closes each of the app's windows. |
| `bricolo.app.set_hidden(app, hidden)` | `true` | Waits. Hides the app's windows, as Cmd+H does, or shows them when `hidden` is `false`. `nil, "failed: no such app"` when none runs. On Linux (X11 only) and Windows it minimizes or restores the app's windows. |
| `bricolo.app.watch(fn)` | `true` | Waits. Starts app events and calls `fn(event)` for each one. An event is `{change, name, bundle_id, pid}`, where `change` is `"launched"`, `"terminated"` or `"activated"` (it came to the front). `bricolo.app.watch(nil)` stops them. macOS and Windows; answers `"unavailable"` elsewhere. |

## audio

Plays sound files on this device's default output. Always runs on this node. A plugin may play several sounds at once and controls only its own; they stop when it stops or reloads.

AVAudioPlayer on macOS, ffmpeg on Linux, `unavailable` on Windows. Plays files in the plugin's data folder, or under a `disk:` grant. The node stops a plugin's sounds when it reloads; keep "playing" in `kv` and play again in `init`. Never runs on a peer.

```wit
/// Plays sound files on this device's default output. Always runs on this
/// node. A plugin may play several sounds at once and controls only its
/// own; they stop when it stops or reloads.
interface audio {
  use types.{call-error};

  record play-options {
    /// 0 to 100; more counts as 100.
    volume: u8,
    /// Starts again from the beginning at the end, without a gap, until
    /// `stop`.
    looping: bool,
  }

  record sound {
    id: u64,
    path: string,
    volume: u8,
    looping: bool,
  }

  /// Starts playing the file at `path`: WAV, AIFF, MP3, AAC or M4A, and on
  /// Linux whatever ffmpeg reads. Windows has no backend yet and answers
  /// `unavailable`. The path must be readable under one of
  /// the plugin's `disk:` grants or in its data folder. Returns the
  /// sound's id.
  play: async func(path: string, options: play-options) -> result<u64, call-error>;
  /// Changes the volume of a sound that is playing.
  set-volume: async func(id: u64, volume: u8) -> result<_, call-error>;
  /// Stops a sound. Stopping one that has ended does nothing.
  stop: async func(id: u64) -> result<_, call-error>;
  /// The caller's sounds that are still playing.
  sounds: async func() -> result<list<sound>, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.audio.play(path[, {volume=, looping=}])` | sound id | Waits until the sound starts. Plays a WAV, AIFF, MP3, AAC or M4A file on the default output: AVAudioPlayer on macOS, ffmpeg on Linux, `"unavailable"` on Windows. `volume` is 0 to 100 (default 100). `looping = true` starts again at the end, with no gap, until `stop`. `~/` expands. Up to 16 sounds at once per script. |
| `bricolo.audio.set_volume(id, volume)` | `true` | Waits. Changes the volume while the sound plays. On Linux the sound restarts from the beginning. |
| `bricolo.audio.stop(id)` | `true` | Waits. Stopping a sound that has ended does nothing. |
| `bricolo.audio.sounds()` | list of `{id, path, volume, looping}` | Waits. This script's sounds that still play. |

## camera

Photos from this device's camera, taken with ffmpeg like `mic`. Needs the Camera permission on macOS; without it calls answer `failed`. Always runs on this node.

```wit
/// Photos from this device's camera, taken with ffmpeg like `mic`. Needs
/// the Camera permission on macOS; without it calls answer `failed`.
/// Always runs on this node.
interface camera {
  use types.{call-error};

  /// Takes one photo with the default camera, saves it as a JPEG in the
  /// plugin's data folder and returns its path.
  snapshot: async func() -> result<string, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.camera.snapshot()` | file path | Waits. Takes one photo with the default camera through ffmpeg: `avfoundation` on macOS (Camera permission), V4L2 on Linux, DirectShow on Windows. A JPEG in `data/<name>.lua/camera/`. The newest 20 photos are kept. ffmpeg is stopped after 10 seconds (`timeout`). `[camera] device` and `ffmpeg` in node.toml choose the device and program. |

## claude-code

Claude Code sessions on this node: start one on a task in a folder, follow its state, send it more work, stop it. The node runs the `claude` CLI headless. Folders must be under `[claude-code] dirs` in node.toml; without that section every call answers `unavailable`.

Needs `[claude-code] dirs` in node.toml (the folders sessions may work in) and the `claude` CLI on the node; without them calls answer `unavailable`. In Lua it is `bricolo.claude_code`. Can run on a peer through rules.toml.

```wit
/// Claude Code sessions on this node: start one on a task in a folder,
/// follow its state, send it more work, stop it. The node runs the
/// `claude` CLI headless. Folders must be under `[claude-code] dirs` in
/// node.toml; without that section every call answers `unavailable`.
interface claude-code {
  use types.{call-error};

  enum session-state {
    /// Stopped before its turn ended.
    idle,
    /// Claude Code is working on a turn.
    working,
    /// The turn ended, but Claude Code was refused a permission it asked
    /// for. Send it more work to go on.
    waiting,
    /// The turn ended.
    finished,
    /// The turn ended with an error, or Claude Code exited without one.
    failed,
  }

  record session {
    /// Claude Code's session id.
    id: string,
    /// The folder it works in.
    dir: string,
    state: session-state,
    /// The text of the last finished turn, if any. For a failed turn,
    /// the error.
    last-result: option<string>,
    /// The first line of the prompt that started the session.
    title: string,
    /// When it started, in unix seconds.
    started: u64,
    /// When its state last changed, in unix seconds.
    updated: u64,
  }

  /// Starts a session on `prompt` in `dir` and answers once Claude Code
  /// has given it an id, while it works. `dir` outside the allowed
  /// folders is `denied`.
  start: async func(dir: string, prompt: string) -> result<session, call-error>;
  /// Resumes a session that is not working with another prompt. A
  /// working session answers `failed`.
  send: async func(id: string, prompt: string) -> result<session, call-error>;
  /// The sessions started since the node started.
  %list: async func() -> result<list<session>, call-error>;
  status: async func(id: string) -> result<session, call-error>;
  /// Stops the session's turn, if one runs. Its state becomes `idle`.
  stop: async func(id: string) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.claude_code.start(dir, prompt)` | session | Waits until Claude Code names the session (at most 25 seconds), not for the work. Runs the `claude` CLI headless in `dir`, which must be under `[claude-code] dirs` in node.toml (else `"denied"`; without the section `"unavailable"`). A session is `{id, dir, state, last_result, title, started, updated}`; `state` is `"working"`, `"finished"`, `"waiting"` (permissions were refused), `"failed"` or `"idle"`. |
| `bricolo.claude_code.send(id, prompt)` | session | Waits. Resumes a session that is not working with another prompt; a working one answers `"failed: session is working"`. |
| `bricolo.claude_code.list()` | list of sessions | Waits. The sessions started since the node started. |
| `bricolo.claude_code.status(id)` | session | Waits. `last_result` holds the text of the last finished turn. |
| `bricolo.claude_code.stop(id)` | `true` | Waits. Kills the running turn, if any; the session becomes `"idle"`. |

## clipboard

The system clipboard: text, images and copied files.

macOS, Linux (X11 or Wayland) and Windows. Elsewhere `unavailable`. Images come back as a PNG path in the plugin's data folder; `set-image` and `set-files` take paths in the data folder or under a `disk:` grant.

```wit
/// The system clipboard: text, images and copied files.
interface clipboard {
  use types.{call-error};

  get: async func() -> result<option<string>, call-error>;
  set: async func(text: string) -> result<_, call-error>;
  /// What the clipboard holds now, among `text`, `image` and `files`.
  formats: async func() -> result<list<string>, call-error>;
  /// Saves the clipboard's image as a PNG in the caller's data folder and
  /// returns its path, or none when it holds no image.
  get-image: async func() -> result<option<string>, call-error>;
  /// Puts the image in a PNG or JPEG file on the clipboard. The path must
  /// be inside the caller's disk grants or its data folder.
  set-image: async func(path: string) -> result<_, call-error>;
  /// The paths of the files copied in the file manager, if any.
  get-files: async func() -> result<list<string>, call-error>;
  /// Copies files, as the file manager does, so a paste drops them.
  set-files: async func(paths: list<string>) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.clipboard.get()` | text or `nil` | Waits. `nil` with no error means the clipboard holds no text. |
| `bricolo.clipboard.set(text)` | `true` | Waits. |
| `bricolo.clipboard.formats()` | list of `"text"`, `"image"`, `"files"` | Waits. |
| `bricolo.clipboard.get_image()` | path or `nil` | Waits. Saves the image as a PNG in the script's data folder. |
| `bricolo.clipboard.set_image(path)` | `true` | Waits. A PNG or JPEG file. |
| `bricolo.clipboard.get_files()` | list of paths | Waits. The files copied in the file manager. |
| `bricolo.clipboard.set_files(paths)` | `true` | Waits. Copies files so a paste drops them. |

## device

The volume, brightness, Wi-Fi, appearance, Focus, lock and sleep of this device. Always runs on this node. A setting the system has no way to change here answers `unavailable`.

macOS, Linux (wpctl or pactl, brightnessctl, nmcli, gsettings, loginctl) and Windows (no Focus). A setting the system can't change answers `unavailable`. `lock`, `sleep` and `set-wifi` can cut the node off. Never routed.

```wit
/// The volume, brightness, Wi-Fi, appearance, Focus, lock and sleep of
/// this device. Always runs on this node. A setting the system has no way
/// to change here answers `unavailable`.
interface device {
  use types.{call-error};

  /// Output volume, 0 to 100.
  volume: async func() -> result<u8, call-error>;
  set-volume: async func(level: u8) -> result<_, call-error>;
  muted: async func() -> result<bool, call-error>;
  set-muted: async func(muted: bool) -> result<_, call-error>;
  /// Brightness of the built-in display, 0 to 100.
  brightness: async func() -> result<u8, call-error>;
  set-brightness: async func(level: u8) -> result<_, call-error>;
  /// Whether Wi-Fi is on.
  wifi: async func() -> result<bool, call-error>;
  /// Turns Wi-Fi on or off. Off can cut this node off from its peers and
  /// from the agent driving it.
  set-wifi: async func(on: bool) -> result<_, call-error>;
  /// Whether the system shows dark windows.
  dark-mode: async func() -> result<bool, call-error>;
  set-dark-mode: async func(on: bool) -> result<_, call-error>;
  /// Whether Do Not Disturb, or Focus on macOS, holds notifications back.
  focus-mode: async func() -> result<bool, call-error>;
  /// On macOS it runs the shortcuts `bricolo focus on` and `bricolo focus off`,
  /// which the user makes once in Shortcuts with "Set Focus".
  set-focus-mode: async func(on: bool) -> result<_, call-error>;
  /// Locks the screen. The user needs their password to come back.
  lock: async func() -> result<_, call-error>;
  /// Puts the device to sleep. The node stops answering until it wakes.
  sleep: async func() -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.device.volume()`, `muted()`, `brightness()`, `wifi()`, `dark_mode()`, `focus_mode()` | number 0 to 100, or boolean | Waits. |
| `bricolo.device.set_volume(level)`, `set_muted(on)`, `set_brightness(level)`, `set_wifi(on)`, `set_dark_mode(on)`, `set_focus_mode(on)` | `true` | Waits. `"unavailable"` where the system has no way to do it. |
| `bricolo.device.lock()`, `bricolo.device.sleep()` | `true` | Waits. |

## disk

Files outside the plugin's own data folder, limited to the paths granted at install (`disk:~/Notes`, `disk:read:~/Documents`). Paths are absolute or start with `~/`.

plugin.toml also needs a grant per path: `permissions = ["disk:~/Notes", "disk:read:~/Documents"]`.

```wit
/// Files outside the plugin's own data folder, limited to the paths
/// granted at install (`disk:~/Notes`, `disk:read:~/Documents`).
/// Paths are absolute or start with `~/`.
interface disk {
  use types.{call-error};

  read: async func(path: string) -> result<list<u8>, call-error>;
  /// Creates or replaces the file. Needs a read-write grant.
  write: async func(path: string, data: list<u8>) -> result<_, call-error>;
  /// Names of the entries in a directory, sorted.
  %list: async func(path: string) -> result<list<string>, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.disk.read(path)` | file contents | Waits. Paths are absolute or start with `~/`. |
| `bricolo.disk.write(path, data)` | `true` | Waits. Creates or replaces the file. |
| `bricolo.disk.list(path)` | sorted list of names | Waits. |

## files

Watches folders for changes. A plugin may watch only paths under its `disk:` grants. Always runs on this node.

```wit
/// Watches folders for changes. A plugin may watch only paths under its
/// `disk:` grants. Always runs on this node.
interface files {
  use types.{call-error};

  /// Watches `path` and everything under it, and returns an id. Each
  /// change arrives as a `file` event carrying that id. A missing path
  /// answers `failed`.
  watch: async func(path: string) -> result<u64, call-error>;
  /// Stops a watch. Stopping one that is already stopped does nothing.
  unwatch: async func(id: u64) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.files.watch(path, fn)` | watch id | Waits. Watches `path` and everything under it, which must lie under a granted path (scripts can reach any). Calls `fn(event)` with `{watch, path, change}`, where `change` is `"created"`, `"modified"` or `"removed"`. A missing path answers `"failed"`. |
| `bricolo.files.unwatch(id)` | `true` | Waits. Stopping a watch that has stopped does nothing. |

## hotkey

Global keyboard shortcuts.

macOS, Linux on X11, and Windows (`cmd` is the Windows key). Elsewhere `unavailable`. A chord another program holds answers `failed`.

```wit
/// Global keyboard shortcuts.
interface hotkey {
  use types.{call-error};

  /// Binds `chord`, written like "cmd+alt+h", under `id`. Each press
  /// delivers `hotkey(id)` to this plugin, and each release
  /// `hotkey-released(id)` on a node that sees releases. "fn" binds the Fn
  /// (Globe) key alone, for push-to-talk; a node that can't bind it
  /// answers `unavailable`.
  bind: async func(id: string, chord: string) -> result<_, call-error>;
  unbind: async func(id: string) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.hotkey.bind(chord, fn[, on_release])` | binding id | Waits. `chord` looks like `"cmd+alt+h"`. Each press calls `fn(id)`; on macOS each release calls `on_release(id)`, for hold-to-talk. `"fn"` binds the Fn (Globe) key alone, on macOS only, and needs Input Monitoring. |
| `bricolo.hotkey.bind(mods, key, fn[, on_release])` | binding id | The Hammerspoon form: `bind({"cmd", "alt"}, "h", fn)`. |
| `bricolo.hotkey.unbind(id)` | `true` | Waits. |

## http

HTTP requests to the `host:port` pairs listed under `[http] allow` in plugin.toml. Others answer `denied`. Redirects are not followed: a 3xx comes back as the response.

plugin.toml also needs `[http] allow = ["api.example.com:443"]` (lowercase host:port). Scripts may reach any host.

```wit
/// HTTP requests to the `host:port` pairs listed under `[http] allow` in
/// plugin.toml. Others answer `denied`. Redirects are not followed: a 3xx
/// comes back as the response.
interface http {
  use types.{call-error};

  record request {
    /// "GET", "POST"…
    method: string,
    /// An absolute http or https URL.
    url: string,
    headers: list<tuple<string, string>>,
    body: list<u8>,
    /// Default 30 s, at most 300 s.
    timeout-ms: option<u32>,
  }

  record response {
    status: u16,
    headers: list<tuple<string, string>>,
    body: list<u8>,
  }

  /// Any status is a response; only transport failures are errors.
  send: async func(request: request) -> result<response, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.http.request(url)` | `{status, headers, body}` | Waits. A GET. Any status is an answer; only a transport problem is an error. |
| `bricolo.http.request{url=, method=, headers=, body=, timeout_ms=}` | `{status, headers, body}` | Waits. `method` defaults to `"GET"`, `headers` is a table of names to values. In the answer, header names are lowercase and repeated headers are joined with `", "`. |

## keyboard

Synthetic key presses on this device: shortcuts and typed text. Always runs on this node, never on a peer. On macOS it needs the Accessibility permission (else `denied`); on Linux an X11 session with the XTEST extension (on Wayland and without a display, `unavailable`). On Windows it answers `denied` when the focused app runs as administrator.

macOS (needs the Accessibility permission, else `denied`), Linux on X11 (XTEST), and Windows (`denied` when the focused app runs as administrator). Elsewhere `unavailable`. Never runs on a peer.

```wit
/// Synthetic key presses on this device: shortcuts and typed text. Always
/// runs on this node, never on a peer. On macOS it needs the Accessibility
/// permission (else `denied`); on Linux an X11 session with the XTEST
/// extension (on Wayland and without a display, `unavailable`). On Windows
/// it answers `denied` when the focused app runs as administrator.
interface keyboard {
  use types.{call-error};

  /// Presses `key` with `mods` held, then lets go of both. Names follow
  /// hotkey chords: mods are "cmd", "ctrl", "alt" and "shift"; keys are
  /// "a" to "z", "0" to "9", "f1" to "f20", "left", "enter", "tab",
  /// "escape", "space", "delete" and the like. Keys are physical positions
  /// on a US layout.
  keystroke: async func(mods: list<string>, key: string) -> result<_, call-error>;
  /// Types `text` into the focused app one key press per character, as a
  /// person types, with no modifiers held. At most 20000 characters.
  type-text: async func(text: string) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.keyboard.keystroke(mods, key)` | `true` | Waits. Presses `key` with `mods` held, like `keystroke({"cmd"}, "v")`. Names follow hotkey chords. Keys are positions on a US layout. |
| `bricolo.keyboard.keystroke(chord)` | `true` | The chord form: `keystroke("cmd+v")`. |
| `bricolo.keyboard.type_text(text)` | `true` | Waits. Types `text` into the focused app one key press per character, with no modifiers held, about 80 characters a second on macOS. Line breaks press Enter and tabs press Tab. At most 20000 characters. |

## kv

A small key/value store private to each plugin, kept in its data folder. It survives restarts and reloads.

```wit
/// A small key/value store private to each plugin, kept in its data folder.
/// It survives restarts and reloads.
interface kv {
  use types.{call-error};

  get: func(key: string) -> option<string>;
  set: func(key: string, value: string) -> result<_, call-error>;
  delete: func(key: string) -> result<_, call-error>;
  keys: func() -> list<string>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.kv.get(key)` | string or `nil` | Each script has its own store, in `data/<name>.lua/kv.json`. Values are strings. |
| `bricolo.kv.set(key, value)` | `true` | Keys are 1 to 256 bytes, values at most 1 MiB. |
| `bricolo.kv.delete(key)` / `bricolo.kv.keys()` | `true` / list | |

## log

Plugin logging. Lines go to the node's daily log file.

```wit
/// Plugin logging. Lines go to the node's daily log file.
interface log {
  enum level {
    trace,
    debug,
    info,
    warn,
    error,
  }

  write: func(level: level, message: string);
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.log.trace/debug/info/warn/error(...)` | nothing | Joins its arguments with spaces. `print(...)` logs at info level. |

## mic

Records the microphone of this device into the caller's data folder, through ffmpeg. Always runs on this node. One recording at a time per plugin; `bricolo status` lists who is recording under `active_sensors`.

Needs ffmpeg (`[mic] ffmpeg` and `device` in node.toml; DirectShow on Windows). On macOS the app running `bricolo run` needs the Microphone permission. Files go to the plugin's data folder, which `speech.transcribe-file` accepts without a `disk:` grant. Never runs on a peer.

```wit
/// Records the microphone of this device into the caller's data folder,
/// through ffmpeg. Always runs on this node. One recording at a time per
/// plugin; `bricolo status` lists who is recording under `active_sensors`.
interface mic {
  use types.{call-error};

  enum audio-format {
    /// WAV, 16 kHz, mono, 16-bit samples: what speech servers expect.
    wav16k-mono,
  }

  record record-options {
    /// The recording stops by itself after this many seconds, at most
    /// 600. 0 means 600.
    max-secs: u32,
    format: audio-format,
  }

  /// Starts recording. Returns the recording's id.
  record-start: async func(options: record-options) -> result<u64, call-error>;
  /// Stops the recording and returns the path of its file. The file is in
  /// the plugin's data folder (`/data/recordings/` inside the plugin), so
  /// `speech.transcribe-file` accepts it. The newest 10 recordings are kept.
  record-stop: async func(id: u64) -> result<string, call-error>;
  /// Stops the recording and deletes its file.
  record-cancel: async func(id: u64) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.mic.record_start([{max_secs=, format=}])` | recording id | Waits until audio arrives. Records the microphone through ffmpeg. `max_secs` (at most 600, the default) stops the recording by itself. `format` can only be `"wav16k-mono"`. One recording at a time per script. |
| `bricolo.mic.record_stop(id)` | file path | Waits. A 16 kHz mono WAV in `data/<name>.lua/recordings/`. The newest 10 recordings are kept. |
| `bricolo.mic.record_cancel(id)` | `true` | Waits. Stops the recording and deletes its file. |

## mouse

The mouse pointer of this device. Points use the same coordinates as `window`. Moving and clicking need the Accessibility permission on macOS; without it they answer `denied`. Always runs on this node.

macOS (needs the Accessibility permission, else `denied`), Linux on X11 (XTEST), and Windows. Elsewhere `unavailable`. Never runs on a peer.

```wit
/// The mouse pointer of this device. Points use the same coordinates as
/// `window`. Moving and clicking need the Accessibility permission on
/// macOS; without it they answer `denied`. Always runs on this node.
interface mouse {
  use types.{call-error};

  record point {
    x: s32,
    y: s32,
  }

  enum button {
    left,
    right,
    middle,
  }

  /// Where the pointer is.
  position: async func() -> result<point, call-error>;
  /// Moves the pointer without clicking.
  move-to: async func(to: point) -> result<_, call-error>;
  /// Clicks `button` at the pointer, `count` times in a row: 2 is a double
  /// click. `count` is 1 to 3; anything else answers `failed`.
  click: async func(button: button, count: u8) -> result<_, call-error>;
  /// Scrolls by whole lines: positive `dy` scrolls down, positive `dx`
  /// scrolls right. On Windows a line is one wheel notch, which scrolls
  /// what the user set in Mouse settings, three lines by default.
  scroll: async func(dx: s32, dy: s32) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.mouse.position()` | `{x, y}` | Waits. Where the pointer is, in `window`'s coordinates. |
| `bricolo.mouse.move_to(x, y)` | `true` | Waits. Moves the pointer without clicking. Any number works: each is rounded to the nearest whole number (half away from zero). NaN or a value outside the 32-bit range answers `"failed: coordinate out of range"`. |
| `bricolo.mouse.click([button [, count]])` | `true` | Waits. Clicks at the pointer. `button` is `"left"` (the default), `"right"` or `"middle"`; any other name raises a Lua error. `count` is 1 to 3 (default 1): 2 is a double click. Any other count answers `"failed: click count must be 1 to 3"`. |
| `bricolo.mouse.scroll(dx, dy)` | `true` | Waits. Scrolls by whole lines: positive `dy` scrolls down, positive `dx` scrolls right. Fractions round like `move_to`, with the same out-of-range answer. |

## notify

Notifications shown to the user.

macOS, Linux (X11 or Wayland) and Windows. Elsewhere `unavailable`.

```wit
/// Notifications shown to the user.
interface notify {
  use types.{call-error};

  show: async func(title: string, body: string) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.notify.show(title [, body])` | `true` | Waits. Shows a notification: macOS's, the desktop's over D-Bus on Linux, or a toast on Windows. |

## process

Programs running on this device, with their CPU and memory use, and stopping or killing one. Always runs on this node. The node refuses to stop itself, its parent and the system's first process.

Every system, through sysinfo. `stop` and `kill` refuse the node and its parent; agents ask before using them. Never routed.

```wit
/// Programs running on this device, with their CPU and memory use, and
/// stopping or killing one. Always runs on this node. The node refuses to
/// stop itself, its parent and the system's first process.
interface process {
  use types.{call-error};

  record process-info {
    pid: u32,
    parent: option<u32>,
    name: string,
    /// The program and its arguments, when the system shows them.
    command: list<string>,
    /// The owner's user name.
    user: option<string>,
    /// Percent of one core over the last second; can pass 100.
    cpu: f32,
    /// Resident memory in bytes.
    memory: u64,
    /// Start time, in seconds since 1970.
    started: u64,
  }

  /// Every process the node can see, the busiest first.
  %list: async func() -> result<list<process-info>, call-error>;
  /// One process, or none when it is gone.
  get: async func(pid: u32) -> result<option<process-info>, call-error>;
  /// Asks a process to quit, as SIGTERM does, so it can save first. On
  /// Windows it closes the process's windows, and ends a process with
  /// none.
  stop: async func(pid: u32) -> result<_, call-error>;
  /// Ends a process at once, as SIGKILL does: unsaved work is lost.
  kill: async func(pid: u32) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.process.list()` | list of `{pid, parent, name, command, user, cpu, memory, started}` | Waits about a quarter second for CPU use. |
| `bricolo.process.get(pid)` | process or `nil` | Waits. |
| `bricolo.process.stop(pid)` | `true` | Waits. SIGTERM; closes the windows on Windows. Refuses the node and its parent. |
| `bricolo.process.kill(pid)` | `true` | Waits. SIGKILL. |

## screen

Pictures of this device's screens. Needs the Screen Recording permission on macOS; without it calls answer `denied`. Always runs on this node.

macOS (needs the Screen Recording permission, else `denied`), Linux on X11, Wayland with `grim`, and Windows. Elsewhere `unavailable`. Never runs on a peer.

```wit
/// Pictures of this device's screens. Needs the Screen Recording
/// permission on macOS; without it calls answer `denied`. Always runs on
/// this node.
interface screen {
  use types.{call-error};
  use window.{rect};

  /// Saves a PNG of `area`, or of the whole screen when none (the main
  /// screen on macOS, every monitor on Linux and Windows), in the
  /// plugin's data folder, and returns its path. An empty area answers
  /// `failed`.
  capture: async func(area: option<rect>) -> result<string, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.screen.capture([area])` | file path | Waits. Saves a PNG of the main screen (every monitor on Linux and Windows), or of `area` (`{x, y, w, h}`, where `width` and `height` also work), in `data/<name>.lua/screenshots/<ms>.png`. A zero width or height answers `"failed: the area is empty"`. |

## scripting

Runs AppleScript, JavaScript for Automation and Shortcuts on macOS, and PowerShell on Windows (and wherever `pwsh` is installed). Always runs on this node. A script can do anything the user can, so it is as trusted as `shell`. On macOS each app a script drives asks the user once for Automation access. A script that runs longer than a minute is stopped and answers `timeout`.

AppleScript, JXA and Shortcuts on macOS (each driven app asks for Automation once); PowerShell on Windows or wherever `pwsh` is installed. As trusted as `shell`. Scripts stop after a minute. Never routed.

```wit
/// Runs AppleScript, JavaScript for Automation and Shortcuts on macOS, and
/// PowerShell on Windows (and wherever `pwsh` is installed). Always runs
/// on this node. A script can do anything the user can, so it is as
/// trusted as `shell`. On macOS each app a script drives asks the user
/// once for Automation access. A script that runs longer than a minute
/// is stopped and answers `timeout`.
interface scripting {
  use types.{call-error};

  /// Runs an AppleScript and returns what it returns, as text.
  run-applescript: async func(source: string) -> result<string, call-error>;
  /// Runs JavaScript for Automation and returns its result, as text.
  run-jxa: async func(source: string) -> result<string, call-error>;
  /// The names of the user's shortcuts.
  list-shortcuts: async func() -> result<list<string>, call-error>;
  /// Runs a shortcut, with `input` as its text input, and returns its
  /// output as text.
  run-shortcut: async func(name: string, input: option<string>) -> result<string, call-error>;
  /// Runs a PowerShell script and returns its output.
  run-powershell: async func(source: string) -> result<string, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.scripting.run_applescript(source)`, `run_jxa(source)` | text | Waits, at most a minute. macOS. |
| `bricolo.scripting.list_shortcuts()` | list of names | Waits. macOS. |
| `bricolo.scripting.run_shortcut(name, input)` | text | Waits. `input` may be `nil`. |
| `bricolo.scripting.run_powershell(source)` | text | Waits. `powershell.exe` on Windows, `pwsh` elsewhere. |

## secrets

Small secrets such as API keys, kept in the macOS Keychain, a Linux Secret Service or the Windows Credential Manager. Each plugin sees only its own. A name is 1 to 128 letters, digits, `.`, `_` or `-`; any other name answers `failed`. Always runs on this node.

The macOS Keychain (needs `bricolo run` in a GUI session), on Linux the Secret Service (GNOME Keyring, KWallet, KeePassXC), service `bricolo`, or the Windows Credential Manager; each plugin sees only its own secrets. Without them `unavailable`. Names are 1 to 128 letters, digits, `.`, `_` or `-`. Never runs on a peer.

```wit
/// Small secrets such as API keys, kept in the macOS Keychain, a Linux
/// Secret Service or the Windows Credential Manager. Each plugin sees only its own. A name is 1 to 128 letters, digits, `.`, `_` or `-`;
/// any other name answers `failed`. Always runs on this node.
interface secrets {
  use types.{call-error};

  /// The secret, or none when it is not set.
  get: async func(name: string) -> result<option<string>, call-error>;
  set: async func(name: string, value: string) -> result<_, call-error>;
  /// Deleting a secret that is not set does nothing.
  delete: async func(name: string) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.secrets.get(name)` | string or `nil` | Waits. The secret, or `nil` when it is not set. Kept in the macOS Keychain, in the login keychain under the service `bricolo`, on Linux in the Secret Service under the same service, or on Windows in the Credential Manager as `bricolo/<script>.lua/<name>`; each script sees only its own. A name is 1 to 128 letters, digits, `.`, `_` or `-`; any other name answers `"failed"`. |
| `bricolo.secrets.set(name, value)` | `true` | Waits. Stores or replaces the secret. |
| `bricolo.secrets.delete(name)` | `true` | Waits. Deleting a secret that is not set does nothing. |

## selection

The text selected in the frontmost app. Needs the Accessibility permission on macOS; without it calls answer `denied`. Always runs on this node.

```wit
/// The text selected in the frontmost app. Needs the Accessibility
/// permission on macOS; without it calls answer `denied`. Always runs on
/// this node.
interface selection {
  use types.{call-error};

  /// The selected text, or none when nothing is selected or the app does
  /// not say.
  get-text: async func() -> result<option<string>, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.selection.get_text()` | text or `nil` | Waits. The text selected in the frontmost app, or `nil` when nothing is selected or the app does not say. |

## shell

Runs programs listed under `[shell] allow` in plugin.toml. No shell is involved: `program` is an exact absolute path from that list, started with `args`. The program gets HOME, a fixed PATH, and `env`, whose names must be listed under `[shell] env`. Anything else answers `denied`. A program that runs past 60 seconds is killed and the call answers `timeout`.

plugin.toml also needs `[shell] allow = ["/usr/bin/say"]` (exact absolute paths) and, for variables the plugin sets, `env = ["NAME"]`.

```wit
/// Runs programs listed under `[shell] allow` in plugin.toml. No shell is
/// involved: `program` is an exact absolute path from that list, started
/// with `args`. The program gets HOME, a fixed PATH, and `env`, whose names
/// must be listed under `[shell] env`. Anything else answers `denied`.
/// A program that runs past 60 seconds is killed and the call answers
/// `timeout`.
interface shell {
  use types.{call-error};

  record output {
    /// None when a signal ended the program.
    code: option<s32>,
    /// The first MiB of each stream, as UTF-8 (invalid bytes replaced).
    stdout: string,
    stderr: string,
  }

  run: async func(program: string, args: list<string>, env: list<tuple<string, string>>) -> result<output, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.shell.run(program [, args [, env]])` | `{code, stdout, stderr}` | Waits. `program` is an absolute path, run directly with no shell, in `/`. `args` is a list of strings and `env` a table of names to values. `code` is `nil` when a signal ended the program. |

## speech

Speech to text.

Needs a `[speech]` server in node.toml; without one calls answer `unavailable`. `transcribe-file` paths must be under a `disk:` grant.

```wit
/// Speech to text.
interface speech {
  use types.{call-error};

  /// Transcribes `audio`. `format` names its encoding as a file
  /// extension, such as "wav".
  transcribe: async func(audio: list<u8>, format: string) -> result<string, call-error>;
  /// Transcribes an audio file. The path must be readable under one of the
  /// plugin's `disk:` grants.
  transcribe-file: async func(path: string) -> result<string, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.speech.transcribe(path [, format])` | text | Waits. Reads the audio file on this node, even when `speech` runs on a peer. `format` defaults to the file extension. |
| `bricolo.speech.transcribe_audio(bytes, format)` | text | Waits. Transcribes audio already in a string. |
| `bricolo.speech.transcribe_file(path)` | text | Waits. The speech server reads the file itself. When `speech` routes to a peer, the node reads the file and sends the bytes. |

## system

The state of this device. Always runs on this node.

macOS (needs `bricolo run`'s main-thread loop), Linux (power from /sys; idle time from X11 or logind; keep-awake and events through logind) and Windows. Elsewhere `unavailable`. Never runs on a peer.

```wit
/// The state of this device. Always runs on this node.
interface system {
  use types.{call-error};

  /// Named `power-info` because `power` is the function below.
  record power-info {
    /// 0 to 100, or none without a battery.
    battery: option<u8>,
    /// On mains power. Always true on a desktop.
    plugged-in: bool,
  }

  /// Seconds since the last key press, click or pointer move.
  idle-seconds: async func() -> result<u64, call-error>;
  power: async func() -> result<power-info, call-error>;
  /// Keeps the device from idle sleep while `on`, until `keep-awake(false)`
  /// or the plugin stops.
  keep-awake: async func(on: bool) -> result<_, call-error>;
  /// Starts or stops delivering `system` events to this plugin.
  watch: async func(on: bool) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.system.watch(fn)` | `true` | Waits. Calls `fn(name)` with `"will_sleep"`, `"did_wake"`, `"screen_locked"`, `"screen_unlocked"` or `"screens_changed"`. `bricolo.system.watch(nil)` stops it. |
| `bricolo.system.idle_seconds()` | number | Waits. Seconds since the last key press, click or pointer move. |
| `bricolo.system.power()` | `{battery, plugged_in}` | Waits. `battery` is 0 to 100, or `nil` without a battery. |
| `bricolo.system.keep_awake(on)` | `true` | Waits. Keeps the device from idle sleep until `keep_awake(false)` or the script stops or reloads. |

## timer

Timers. `after` and `every` return an id at once; `timer(id)` events follow.

```wit
/// Timers. `after` and `every` return an id at once; `timer(id)` events follow.
interface timer {
  /// Fires once after `ms` milliseconds.
  after: func(ms: u32) -> u64;
  /// Fires every `ms` milliseconds (at least 10) until cancelled.
  every: func(ms: u32) -> u64;
  /// Cancels a timer this plugin started. Unknown ids are ignored.
  cancel: func(id: u64);
  /// Waits `ms` milliseconds without blocking the node.
  sleep: async func(ms: u32);
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.timer.after(ms, fn)` | timer id | Calls `fn(id)` once after `ms` milliseconds. |
| `bricolo.timer.every(ms, fn)` | timer id | Calls `fn(id)` every `ms` milliseconds (at least 10) until cancelled. |
| `bricolo.timer.cancel(id)` | `true` or `false` | `false` when the id is unknown or already fired. |
| `bricolo.timer.sleep(ms)` | `true` | Waits. |

## tts

Text to speech. Speech is queued: `speak` returns once the text is queued. The node splits long text into sentence chunks and speaks them one at a time.

Speaks with `say` on macOS, espeak-ng on Linux and SAPI on Windows unless node.toml names a TTS server.

```wit
/// Text to speech. Speech is queued: `speak` returns once the text is
/// queued. The node splits long text into sentence chunks and speaks them
/// one at a time.
interface tts {
  use types.{call-error};

  record queue-status {
    speaking: bool,
    /// Chunks waiting behind the one being spoken.
    queued: u32,
    /// The node's default voice, if one is set.
    voice: option<string>,
  }

  /// `voice` picks a voice or a profile from node.toml by name; none means
  /// the node's default.
  speak: async func(text: string, voice: option<string>) -> result<_, call-error>;
  /// Stops the current speech and clears the queue.
  stop: async func() -> result<_, call-error>;
  status: async func() -> result<queue-status, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.tts.speak(text [, voice])` | `true` | Waits until the text is queued. |
| `bricolo.tts.stop()` | `true` | Waits. Stops speech and clears the queue. |
| `bricolo.tts.status()` | `{speaking, queued, voice}` | Waits. `voice` is the default voice, or `nil`. |

## ui

A tile of controls in the plugin's hub card, a display-only widget, and a few kinds of floating panel, described as data. The UI process draws them; plugins never ship HTML or JavaScript. What the user does arrives as a `ui` event. Always runs on this node.

Drawn by `bricolo-ui`, the hub, on macOS and Linux. The node keeps the tile, the widget and the panels while no hub runs. Switches, buttons, choices and reviews arrive as the `ui` event.

```wit
/// A tile of controls in the plugin's hub card, a display-only widget, and
/// a few kinds of floating panel, described as data.
/// The UI process draws them; plugins never ship HTML or JavaScript. What
/// the user does arrives as a `ui` event. Always runs on this node.
interface ui {
  use types.{call-error};

  record switch {
    id: string,
    label: string,
    on: bool,
  }

  record button {
    id: string,
    label: string,
  }

  /// This plugin's controls, drawn inside its card in the hub. Toggling a
  /// switch delivers `ui(switch(..))` with its new value; pressing a button
  /// delivers `ui(button(id))`.
  record tile {
    title: string,
    status-text: string,
    switches: list<switch>,
    buttons: list<button>,
  }

  /// One labelled value of a widget, like `Paris` and `20:05`.
  record widget-row {
    label: string,
    value: string,
  }

  /// A display-only view of values worth a glance, in the hub's Widgets
  /// view: the time in other cities, the battery level. It has no switches
  /// or buttons; those go in the tile. Most plugins have none.
  record widget {
    title: string,
    rows: list<widget-row>,
  }

  /// A bar across a widget: `label` and `value` above it, `note` under it.
  /// The bar is split into `marks` equal segments, such as the hours of a
  /// window or the days of a week, and filled to `fill` percent. `now`, in
  /// percent, draws a line across it, such as the time gone in that window.
  record gauge {
    label: string,
    value: string,
    /// 0 to 100.
    fill: u8,
    /// 1 to 32 segments; 1 draws a plain bar.
    marks: u8,
    /// 0 to 100, or none for no line.
    now: option<u8>,
    /// May be empty.
    note: string,
  }

  /// A widget with gauges above its rows.
  record gauge-widget {
    title: string,
    gauges: list<gauge>,
    rows: list<widget-row>,
  }

  enum panel-kind {
    /// Text near the bottom of the screen, like live dictation.
    caption,
    /// A list to pick one entry from. Delivers `ui(choice(..))`.
    choice-list,
    /// A short message that hides itself, after 4 s unless `timeout-ms`.
    toast,
    /// Editable text to accept or cancel. Delivers `ui(review(..))`.
    review,
  }

  record panel {
    kind: panel-kind,
    title: string,
    /// The caption, the toast, or the text to review.
    text: string,
    /// The entries of a choice list; empty for the other kinds.
    choices: list<string>,
    /// Hide after this many milliseconds.
    timeout-ms: option<u32>,
  }

  /// Sets or replaces this plugin's tile.
  set-tile: func(tile: tile) -> result<_, call-error>;
  clear-tile: func() -> result<_, call-error>;
  /// Sets or replaces this plugin's widget. At most 16 rows.
  set-widget: func(widget: widget) -> result<_, call-error>;
  /// Sets or replaces this plugin's widget with gauges. At most 8 gauges
  /// and 16 rows.
  set-gauge-widget: func(widget: gauge-widget) -> result<_, call-error>;
  clear-widget: func() -> result<_, call-error>;
  /// Shows a panel and returns its id. Panels never take focus from the
  /// app in front. Showing a kind this plugin already shows replaces that
  /// panel and keeps its id.
  show-panel: func(panel: panel) -> result<string, call-error>;
  /// Hides one of this plugin's panels. Unknown ids are ignored.
  hide-panel: func(id: string) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.ui.set_tile{title=, status=, switches=, buttons=, on_switch=, on_button=}` | `true` | Sets or replaces this script's tile in the hub. A switch is `{id, label, on}`, a button `{id, label}`. `on_switch(id, on)` runs when the user toggles a switch, `on_button(id)` when they press a button. |
| `bricolo.ui.clear_tile()` | `true` | Removes the tile. |
| `bricolo.ui.set_widget{title=, rows={{label=, value=}}, gauges={{label=, value=, fill=, marks=, now=, note=}}}` | `true` | Sets or replaces this script's widget in the hub's Widgets view: display only, up to 16 rows, each a label and a value, and up to 8 gauges drawn above them. A gauge is a bar of `marks` segments (1 to 32) filled to `fill` percent, with a line at `now` percent if set; `gauges` is optional. Only for values worth a glance; controls go in the tile. |
| `bricolo.ui.clear_widget()` | `true` | Removes the widget. |
| `bricolo.ui.show_panel{kind=, title=, text=, choices=, timeout_ms=, on_choice=, on_review=}` | panel id | Shows a floating panel. `kind` is `"caption"`, `"toast"`, `"choice_list"` or `"review"`. `on_choice(index, label)` gets a 1-based index, or `nil, nil` when the user dismisses the list. `on_review(accepted, text)` gets the edited text. |
| `bricolo.ui.caption(text [, timeout_ms])` / `bricolo.ui.toast(text [, timeout_ms])` | panel id | Shortcuts for `show_panel`. A toast hides after 4 seconds unless `timeout_ms` says otherwise. |
| `bricolo.ui.hide_panel(id)` | `true` | Hides one of this script's panels. Unknown ids are ignored. |

## virtual-screen

Screens that no monitor shows, where apps run and are driven without touching the user's screen, keyboard or pointer. Linux runs each on Xvfb, and `accessibility` reaches the apps on it too. Windows Server runs each in a Remote Desktop session of its own, once an account for it is stored as secrets; other Windows editions use a hidden desktop, pictured window by window and driven by window messages, where key chords with modifiers are refused. macOS answers `unavailable` for now. Always runs on this node.

Linux (Xvfb: launch, capture, click, type) and Windows (a Remote Desktop session on Windows Server with the account stored as the secrets `virtual-screen-rdp-user` and `virtual-screen-rdp-password`, else a hidden desktop with keys without modifiers); macOS answers `unavailable`. `launch` needs the program in the plugin's `[shell] allow`. In Lua it is `bricolo.virtual_screen`. Never routed.

```wit
/// Screens that no monitor shows, where apps run and are driven without
/// touching the user's screen, keyboard or pointer. Linux runs each on
/// Xvfb, and `accessibility` reaches the apps on it too. Windows Server
/// runs each in a Remote Desktop session of its own, once an account for
/// it is stored as secrets; other Windows editions use a hidden desktop,
/// pictured window by window and driven by window messages, where key
/// chords with modifiers are refused. macOS answers
/// `unavailable` for now. Always runs on this node.
interface virtual-screen {
  use types.{call-error};

  record virtual-screen-info {
    id: u32,
    /// The system's name for it: the X display, like `:99`, or the
    /// desktop's name on Windows.
    name: string,
    width: u32,
    height: u32,
    /// The processes `launch` started there that still run.
    pids: list<u32>,
  }

  /// Starts an empty screen of that size, in pixels.
  create: async func(width: u32, height: u32) -> result<virtual-screen-info, call-error>;
  %list: async func() -> result<list<virtual-screen-info>, call-error>;
  /// Starts `program` with `args` on the screen and returns its pid. The
  /// program must be an absolute path the caller's `[shell]` allows.
  launch: async func(id: u32, program: string, args: list<string>) -> result<u32, call-error>;
  /// Saves a PNG of the screen in the caller's data folder and returns
  /// its path.
  capture: async func(id: u32) -> result<string, call-error>;
  /// Clicks at a point, in pixels from the top-left corner.
  click: async func(id: u32, x: s32, y: s32) -> result<_, call-error>;
  /// Types text into whatever has focus on that screen.
  type-text: async func(id: u32, text: string) -> result<_, call-error>;
  /// Presses a key or chord, like `return` or `ctrl+s`.
  press-key: async func(id: u32, key: string) -> result<_, call-error>;
  /// Ends the screen and every program running on it.
  destroy: async func(id: u32) -> result<_, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.virtual_screen.create(width, height)` | `{id, name, width, height, pids}` | Waits. Linux (Xvfb) and Windows (hidden desktop). |
| `bricolo.virtual_screen.list()` | list of screens | Waits. Only this script's screens. |
| `bricolo.virtual_screen.launch(id, program, args)` | pid | Waits. `program` is an absolute path. |
| `bricolo.virtual_screen.capture(id)` | path | Waits. A PNG in the script's data folder. |
| `bricolo.virtual_screen.click(id, x, y)`, `type_text(id, text)`, `press_key(id, key)` | `true` | Waits. |
| `bricolo.virtual_screen.destroy(id)` | `true` | Waits. Ends the screen and its programs. |

## window

Windows and screens of this device. Points are measured from the top-left corner of the main display, y growing downward. Needs the Accessibility permission on macOS; without it calls answer `denied`.

macOS (needs the Accessibility permission, else `denied`), Linux on X11, and Windows (physical pixels). Elsewhere `unavailable`.

```wit
/// Windows and screens of this device. Points are measured from the
/// top-left corner of the main display, y growing downward. Needs the
/// Accessibility permission on macOS; without it calls answer `denied`.
interface window {
  use types.{call-error};

  record rect {
    x: s32,
    y: s32,
    width: u32,
    height: u32,
  }

  record window-info {
    /// Stable while the window exists.
    id: u64,
    app: string,
    title: string,
    frame: rect,
  }

  record screen {
    id: u32,
    frame: rect,
    /// The frame without the menu bar and the Dock.
    visible-frame: rect,
    /// The screen with the menu bar.
    main: bool,
  }

  /// Windows of the apps that show in the Dock.
  %list: async func() -> result<list<window-info>, call-error>;
  /// The focused window, if any.
  focused: async func() -> result<option<window-info>, call-error>;
  /// Raises the window and brings its app to the front.
  focus: async func(id: u64) -> result<_, call-error>;
  /// Moves and resizes the window.
  set-frame: async func(id: u64, frame: rect) -> result<_, call-error>;
  /// Closes the window, as its close button does. The app may ask to save
  /// first.
  close: async func(id: u64) -> result<_, call-error>;
  /// Minimizes the window to the Dock, or restores it when `minimized` is
  /// false.
  set-minimized: async func(id: u64, minimized: bool) -> result<_, call-error>;
  screens: async func() -> result<list<screen>, call-error>;
}
```

In a script:

| Call | Returns | Notes |
|---|---|---|
| `bricolo.window.list()` | list of windows | Waits. A window is `{id, app, title, frame}`, and a frame is `{x, y, w, h}` in points from the top-left corner of the main display (pixels on Linux and Windows). |
| `bricolo.window.focused()` | window or `nil` | Waits. |
| `bricolo.window.focus(id)` | `true` | Waits. |
| `bricolo.window.set_frame(id, frame)` | `true` | Waits. `frame` takes `w` and `h`, or `width` and `height`. |
| `bricolo.window.close(id)` | `true` | Waits. Presses the window's close button; the app may ask to save first. |
| `bricolo.window.set_minimized(id, minimized)` | `true` | Waits. Minimizes the window to the Dock, or restores it when `minimized` is `false`. |
| `bricolo.window.screens()` | list of screens | Waits. A screen is `{id, frame, visible_frame, main}`. `visible_frame` leaves out the menu bar and the Dock. |

## The plugin world

What a plugin may import and must export. A plugin imports only the interfaces it uses; the node checks those against `plugin.toml` and `permissions.toml` before it runs any code.

```wit
/// What a plugin may import and must export. A plugin imports only the
/// interfaces it uses; the node checks those against `plugin.toml` and
/// `permissions.toml` before it runs any code.
world plugin {
  import log;
  import timer;
  import hotkey;
  import tts;
  import speech;
  import disk;
  import clipboard;
  import kv;
  import window;
  import app;
  import notify;
  import shell;
  import http;
  import ui;
  import keyboard;
  import mic;
  import audio;
  import system;
  import files;
  import mouse;
  import screen;
  import selection;
  import camera;
  import secrets;
  import claude-code;
  import accessibility;
  import process;
  import device;
  import scripting;
  import virtual-screen;

  use types.{event};

  /// Called once after loading. `config` is the plugin's settings as a JSON
  /// object: defaults from `plugin.toml`, overridden by `node.toml`.
  export init: async func(config: string) -> result<_, string>;
  /// Called for each event, one at a time.
  export on-event: async func(event: event);
  /// Called before the plugin stops or reloads.
  export dispose: async func();
}
```

## Script-only

| Call | Returns | Notes |
|---|---|---|
| `bricolo.os()` | string | `"macos"`, `"linux"` or `"windows"`. |
| `bricolo.script` | string | This script's name. |
