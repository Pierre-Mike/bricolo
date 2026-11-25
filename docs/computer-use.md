# Computer use

Five capabilities let a script, a plugin or an agent behind the [MCP gateway](mcp.md) work a desktop: `accessibility` reads and drives app windows, `process` lists and stops programs, `device` changes the volume, brightness, Wi-Fi, appearance and Focus, locks and sleeps, `scripting` runs AppleScript, Shortcuts and PowerShell, and `virtual-screen` runs apps on a screen no monitor shows. `clipboard` also carries images and copied files now. Each one is a WIT interface in `wit/`, so the gateway lists it as tools on every node that has it.

## Rules for agents

The `accessibility` description the gateway sends carries the first two, and the [skill](../skills/create-bricolo-app/SKILL.md) repeats them:

1. Prefer a programmatic path: `shell`, `disk`, `http`, an app's own CLI or `scripting`. They are faster, they can't click the wrong thing, and they leave the user's screen alone. Use `accessibility` only when a window needs controls those can't reach.
2. Never submit, send, buy, delete, or change account settings through an app unless the user asked for that exact action. In an app with private content, read only what the task needs.
3. Read before acting. Take a `snapshot`, act on an index from it, and use the fresh state every action returns. An index belongs to the element, and it goes stale when the element goes away.
4. Trust the verification. `verified` means the node read the new value back. `unverified` means the system took the action and nothing could be read back. `suspected-noop` means nothing changed: don't click the same element again. `mismatch` means the text read back differs, so look before typing again. Never report an unverified action as done.
5. Wait with `wait`, not with sleeps.
6. Ask first before anything marked destructive: `process.stop` and `kill`, `device.lock`, `sleep` and `set-wifi` (each can cut the agent off from the node), the `scripting.run-*` calls and `virtual-screen.launch`. MCP clients see these as `destructiveHint`.

## The accessibility tree

`accessibility.snapshot` returns the window as text, one element per line, indented by depth:

```
1 window Untitled
  3 toolbar
    4 button Bold
  7 text entry area hello world
  9 button (disabled) Save
```

The leading number is the element's index. `click`, `set-value`, `type-text` and `perform` take it. Unnamed groups are left out, and their children move up a level. A button or a menu item shows on one line with its children folded in. A read stops after 1200 elements or 64 levels. Each cut shows `... N more not shown (snapshot limit)`; snapshot the parent's index with `element` to see them. `query` keeps only the lines that contain its text, with their ancestors. `find` returns the matching elements as records with their frame, value and actions. `menu(["File", "Export…"])` picks a menu bar item by name. `press-key` refuses chords that quit, close, log out or lock (cmd+q, alt+F4, ctrl+alt+delete…): use the app's own button or menu item.

## Per system

| | macOS | Windows | Linux |
|---|---|---|---|
| `accessibility` | `AXUIElement`; keys with `CGEventPostToPid`, so the app stays in the background | UI Automation patterns (Invoke, Toggle, Value, RangeValue, ExpandCollapse); keys only while the app is in front | AT-SPI over D-Bus; keys only while the app's window is active |
| Grant | Accessibility; screenshots need Screen Recording | none | the session's accessibility bus |
| `process` | `sysinfo` | `sysinfo`; `stop` runs `taskkill` (closes windows) | `sysinfo` |
| `device` | `osascript`, `networksetup`, `defaults`, `pmset`; brightness and lock through DisplayServices and login.framework; Focus through two shortcuts the user makes, `bricolo focus on` and `bricolo focus off` | PowerShell (Core Audio, WMI brightness, WinRT radios), `reg`, `LockWorkStation`; no Focus | `wpctl` or `pactl`, `brightnessctl`, `nmcli`, `gsettings`, `loginctl`, `systemctl` |
| `scripting` | `osascript`, `shortcuts`, and `pwsh` when installed | `powershell.exe` | `pwsh` when installed |
| `virtual-screen` | not yet: unavailable | on Windows Server with a stored account, a Remote Desktop session of its own; else a hidden desktop (`CreateDesktopW`): launch, capture through `PrintWindow`, click, type and keys without modifiers through window messages | Xvfb: launch, capture, click, type, keys |

On a Mac, the hub's first-run walkthrough, **Set up this Mac** on Home, lists Accessibility as used by `accessibility` and Screen Recording as used by its screenshots, with a button to ask for each. Every macOS grant also shows in the hub's Library under **macOS access**, and the Library card for `accessibility` says it needs Accessibility. Home lists the grant under Needs attention when a running app uses the capability and the node lacks it, with Ask macOS beside it. `scripting` drives other apps through Apple events, so macOS asks once per app the first time a script drives it.

### Virtual screens

- **Linux**: each screen is an Xvfb display, `:99` and up. Programs launched there get `DISPLAY` set to it, and AT-SPI sees them like any app, so `accessibility` drives them too.
- **Windows Server**: a Remote Desktop session of its own. The user makes a local account once, adds it to Remote Desktop Users, and stores its name and password as the secrets `virtual-screen-rdp-user` and `virtual-screen-rdp-password` (`cmdkey /generic:bricolo/virtual-screen-rdp-user /user:bricolo /pass:<name>`, the same for the password). `create` connects `mstsc` to `127.0.0.2` as that account and waits for its session; `launch`, `capture`, `click`, `type-text` and `press-key` run as one-shot scheduled tasks inside it (`schtasks /it`), the only way a node that is not SYSTEM reaches another session; `destroy` signs it out. `schtasks` and `cmdkey` take the password on their command lines, so use an account made for this alone. CI makes a throwaway one on the hosted Windows Server runner.
- **Other Windows editions**: client editions allow one interactive session, and connecting to yourself over RDP needs third-party patches, so the node uses a hidden desktop in the user's own session instead. Programs run there unseen. `SendInput` reaches only the desktop on screen, so a capture asks each window to paint itself (`PrintWindow`) and composes the pictures, and clicks, text and keys go as window messages. Apps that draw with DirectX can come out blank, and chords with modifiers are refused.
- **macOS**: not supported yet. A screen no monitor shows needs the private `CGVirtualDisplay` class, or a virtual machine (Virtualization.framework with a macOS guest). Both are planned, and until then `virtual-screen` answers `unavailable`.

## Demo

Both demos passed on 2026-11-25 with Claude Sonnet 5.5, which had the gateway as its only tools.

- **macOS**: the agent listed TextEdit's windows, took a snapshot and set the document's text (read back as `verified`). It then picked File > Save, and the file on disk held the new text. That took 8 turns and cost $0.07.
- **Linux in Docker**: over HTTP with a token, the agent listed apps, took a snapshot of the zenity dialog and set its entry (`verified`). It clicked OK, which closed the dialog, and zenity printed the agent's text. That took 6 turns and cost $0.05.

On macOS the accessibility API sees only the windows on the current Space. A window behind a full-screen app answers `window_not_found` until the user shows it, the same limit Orca has.

## Where this came from: Orca

The design follows Orca's computer use ([stablyai/orca](https://github.com/stablyai/orca), MIT, `native/computer-use-*` and its `orca computer` CLI). What we studied:

- **Native code**: macOS is a Swift helper (`AXUIElement`, ScreenCaptureKit, background input through per-process events). Linux is a Python runtime on pyatspi (AT-SPI), and Windows is a PowerShell runtime on UIAutomationClient. Each is driven from Orca's Electron app over a local socket.
- **`get-app-state`**: the tree, with a screenshot of the window unless `--no-screenshot` is passed. Actions return the new state, as a diff when the change is small. `--restore-window` brings the window forward for input that only works in front, and the skill forbids it unless the user agrees. `--query`, `--element-index` (a subtree) and `--max-elements` narrow the output.
- **Element IDs**: `orca computer` numbers elements with plain indexes (`12 button Save`) that stay put while the element exists. The `@e3` style belongs to Orca's embedded browser tool (`orca-cli`), not to computer use. We use the plain indexes.
- **Clicks, typing, menus, dialogs**: semantic actions first (AXPress, set-value, select), then coordinates as a fallback. Typing reads the text back, and verification labels separate "verified", "unverified" and "suspected noop". Menus are clicked like other elements, and dialogs and sheets are windows or children of one. Shortcuts that quit, close or lock are blocked.
- **Safety guidance** (its `orca-computer-use` skill): prefer shell, files, git and HTTP, never push, submit, buy or delete without being asked, never report unverified actions as success, and never bring a window forward without asking.

**Decision: reuse the design, not the code.** Orca's backends are Swift, Python and PowerShell processes, and porting them line by line would bring a runtime per system into a Rust node. We kept the design: the line format, the elision and folding rules, the node limits and their markers, stable indexes, verification labels, blocked shortcuts, and the safety rules. The only code ported is the rendering rules from `SnapshotRendering.swift`, in `crates/providers/src/accessibility.rs` (`mod render`), and that file carries Orca's MIT notice. Not ported yet: tree diffs in action results, screenshot scaling flags, browser tab-strip compaction, coordinate clicks and drags, and `launch-app` in the background (the `app` capability launches apps).

## Code

| Path | Job |
|---|---|
| `wit/accessibility.wit`, `process.wit`, `device.wit`, `scripting.wit`, `virtual-screen.wit`, `clipboard.wit` | The interfaces |
| `crates/providers/src/accessibility.rs` | The engine: app picking, the walk, indexes, rendering, actions and verification, menus, waits, blocked chords, and the fake desktop its tests drive |
| `crates/providers/src/{macos,windows,linux}/accessibility.rs` | `AxBackend` for each system |
| `crates/providers/src/process.rs` | Guards over a `ProcessBackend`; `sysinfo` |
| `crates/providers/src/device.rs`, `scripting.rs`, `cmd.rs` | Command plans per system behind a `Runner`; the tests check them with `FakeRunner` |
| `crates/providers/src/virtual_screen.rs`, `{linux,windows}/virtual_screen.rs`, `rdp.rs` | Ownership, grants and limits; Xvfb, hidden desktops, and the Remote Desktop session plans for Windows Server |
| `crates/providers/src/clipboard.rs` | `Rich`: images and files on top of each system's text clipboard, through arboard |
| `crates/mcp/src/catalog.rs` | Which methods are read-only and which are destructive |
| `scripts/demo-computer-use.sh` | The macOS demo (`just demo-computer-use`): Claude Code, with the gateway as its only tools, edits and saves a document in TextEdit through the accessibility tree; the script checks the file on disk. TextEdit's window must be on the current Space: `DEMO_ACTIVATE=1` brings it forward |
| `scripts/demo-computer-use-linux.sh` | The Linux demo (`just demo-computer-use-linux`, any machine with Docker): the node and a zenity dialog run on Xvfb in the linux-test image, Claude Code on the host reaches the HTTP gateway, fills the dialog through AT-SPI and presses OK; the script checks what zenity printed |
