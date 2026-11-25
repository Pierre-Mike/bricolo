# Installing and running as a service

The node is meant to run all the time, started at login. On macOS it is a signed app bundle started by a LaunchAgent (design Decisions 5 and 8). On Linux it is a systemd user service, and on Windows a Task Scheduler task that runs at logon. `bricolo service` writes and loads each of them.

## macOS

### What gets built

`just bundle-macos` builds `target/bundle/bricolo.app`:

```
bricolo.app                     com.pierre-mike.bricolo
├── Contents/MacOS/bricolo-ui                 the hub and the menu bar item
└── Contents/Helpers/bricolo.app              com.pierre-mike.bricolo.node
    └── Contents/MacOS/bricolo                the node; the LaunchAgent runs `bricolo run`
```

- The app is an agent app (`LSUIElement`): a menu bar item and no Dock icon. Its Info.plist comes from `tauri.conf.json` merged with `crates/ui/Info.plist`, which adds the usage strings macOS shows when a plugin first asks for the microphone, the camera or control of another app.
- `bricolo` sits in a helper app with its own bundle id. In an earlier layout it sat in `Contents/MacOS` next to `bricolo-ui`. The running node then counted as the app for LaunchServices, so opening bricolo.app brought the node forward and never started the hub. The helper's Info.plist is `crates/node/Info.plist`, which `build.rs` also embeds in every `bricolo` binary built on macOS.
- The hub's Start node and `bricolo service install --app` find `bricolo` in the helper.

### The signing identity

macOS records each permission grant against the signature of the program that asked. An ad-hoc signature changes with every build, so macOS would ask again after each rebuild. A signature from one stable identity keeps the grants.

```sh
scripts/macos-dev-identity.sh
```

creates "bricolo dev" in the login keychain: a self-signed certificate valid for code signing only, for 10 years, and its private key, which `/usr/bin/codesign` may use without asking. It then asks macOS to trust the certificate for code signing, which can open a password dialog. Signing works without that trust. Running the script again changes nothing. To remove the identity, open Keychain Access, choose the login keychain, then My Certificates, and delete "bricolo dev".

`just bundle-macos` signs with that identity when it exists, else ad-hoc with a warning. `BRICOLO_SIGN_IDENTITY` overrides the choice: a name, a SHA-1 hash, or `-` for ad-hoc. Both bundles get a designated requirement such as:

```
identifier "com.pierre-mike.bricolo.node" and certificate leaf = H"<sha-1 of your identity>"
```

The hardened runtime is off, so there are no entitlements to keep in step. Gatekeeper (`spctl -a -vv`) rejects a bundle signed with a local identity or ad-hoc. It still runs, because it was built on this Mac and carries no quarantine flag. A copy downloaded from a release does carry it: right-click the app and choose Open once, or run `xattr -dr com.apple.quarantine bricolo.app`.

### Installing

```sh
just install-macos            # build, copy to ~/Applications, start the service
just install-macos --ui       # also open the hub's menu bar item at login
```

`just install-macos` copies the bundle to `~/Applications/bricolo.app` (no `sudo`) and runs:

```sh
~/Applications/bricolo.app/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo \
    service install --app ~/Applications/bricolo.app
```

That writes `~/Library/LaunchAgents/com.pierre-mike.bricolo.plist` and loads it with `launchctl bootstrap gui/$UID`. The agent:

- runs `<app>/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo run` at login and once at install (`RunAtLoad`),
- starts it again after a crash, but not after a clean exit (`KeepAlive` with `SuccessfulExit` false), so `bricolo service stop` and SIGTERM stay stopped,
- writes stdout and stderr to `<data>/logs/bricolo-service.log` (`~/.local/share/bricolo/logs/`),
- sets `PATH` to `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin` and the system folders, because launchd starts agents with the system folders only and sidecars often live in the others,
- passes `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR` only when they were set during the install.

`bricolo service install` without `--app` looks for the app in `/Applications`, then in `~/Applications`. `--binary <path>` runs a plain `bricolo` instead, such as `target/release/bricolo`; macOS then records the permissions against that binary. `--dry-run` prints the files and commands without writing or running anything. With `--ui`, a second agent, `com.pierre-mike.bricolo.ui`, opens `bricolo-ui --background` at login. That agent does not restart the hub after you quit it.

To call the bundled `bricolo` as `bricolo`, link it into a folder on your `PATH`:

```sh
ln -sf ~/Applications/bricolo.app/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo ~/.local/bin/bricolo
```

### Every day

| Command | What it does |
|---|---|
| `bricolo service status` | Whether the agent is loaded, its state, pid and last exit code. Exits 2 when it is not loaded |
| `bricolo status` | The running node's plugins, scripts, sidecars and mesh |
| `bricolo service stop` | Unloads the agent (`launchctl bootout`). The node stays stopped until `bricolo service start` or the next login |
| `bricolo service start` | Loads the agent again and starts the node (`bootstrap`, then `kickstart`) |
| `open ~/Applications/bricolo.app` | Opens the hub |

### Permissions

The node runs from the helper app, so macOS lists it as "bricolo node". Open System Settings, then Privacy & Security, and add or turn it on in:

| Setting | Needed for |
|---|---|
| Accessibility | `window` (list, focus and move windows), `keyboard` (type and press keys), `selection`, and `accessibility` (read and drive other apps' windows, as agents do through the MCP gateway). Without it, `window` and `keyboard` answer `denied` and `accessibility` answers `failed: accessibility_error` with this fix in the message |
| Input Monitoring | the `fn` hotkey (hold Fn to talk). Other hotkey chords need no permission |
| Microphone | the microphone capability. macOS asks the first time a plugin records |
| Camera | the `camera` capability |
| Screen Recording | the `screen` capability, and the screenshots `accessibility.snapshot` takes when asked |
| Automation | `scripting` (AppleScript, JavaScript for Automation), `device.set-dark-mode`, and any plugin or script that drives another app through Apple Events. macOS asks once per app |

`bricolo permissions` prints what the running node holds, Microphone, Camera and Screen Recording included, and under `used_by` the capabilities each grant serves. `bricolo permissions --request` has the node ask macOS for what is missing of Accessibility and Input Monitoring, which lists "bricolo node" in those settings and shows each prompt once, then opens the settings panes; turn the entries on there. The first `keyboard` call that macOS refuses asks too. The hub's Library view shows the same grants, with Ask and Revoke… buttons for each ([ui.md](ui.md#the-library)).

If the entry is missing, press the + button, press Cmd+Shift+G and paste:

```
~/Applications/bricolo.app/Contents/Helpers/bricolo.app
```

then turn the new entry on. If calls still answer `denied`, or an entry is on but `bricolo permissions` says `false` (an older build holds the grant), remove the entry with the − button, run `bricolo permissions --request` again, and restart the node with `bricolo service stop` and `bricolo service start`. Because the grant follows the "bricolo dev" signature, rebuilding and reinstalling keeps it. Signed ad-hoc, macOS treats each build as a new program, and you add it again.

The hub (`bricolo-ui`) needs none of these. Plugins and scripts reach the machine through the node.

### Uninstalling

```sh
~/Applications/bricolo.app/Contents/Helpers/bricolo.app/Contents/MacOS/bricolo service uninstall
rm -rf ~/Applications/bricolo.app
```

`bricolo service uninstall` unloads both agents and deletes their files. Your configuration in `~/.config/bricolo` and the node's data in `~/.local/share/bricolo` stay. Remove the grants in System Settings with the − button.

## Linux

### The tarball

`just bundle-linux` builds `dist/bricolo-<version>-linux-<arch>.tar.gz`. On macOS it builds inside the `just linux-test` image, Debian bookworm, so the binaries need glibc 2.36 or later. The tarball holds `bricolo`, `install.sh` and a README. It holds `bricolo-ui` only when `target/release/bricolo-ui` exists, because building the hub needs GTK 3 and WebKitGTK 4.1, which the test image does not have. To build the hub on a Linux desktop, install the packages the CI's Ubuntu job lists ([ui.md](ui.md)) and run `just ui`. The release workflow also builds a `.deb` of `bricolo-ui`, with `bricolo` in `/usr/bin`, and an AppImage, for x86_64.

```sh
tar xzf bricolo-0.1.0-linux-x86_64.tar.gz
cd bricolo-0.1.0-linux-x86_64
./install.sh                       # copy to ~/.local/bin, then bricolo service install
./install.sh --no-service          # only copy
./install.sh --session headless    # arguments go to bricolo service install
```

`PREFIX=/opt/bricolo ./install.sh` installs to `/opt/bricolo/bin` instead.

### The systemd user service

`bricolo service install` writes `~/.config/systemd/user/bricolo.service` (or under `$XDG_CONFIG_HOME`), then runs `systemctl --user daemon-reload`, `enable` and `restart`. The unit runs `<path>/bricolo run` with `Restart=on-failure`, and passes through `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`, `XDG_SESSION_TYPE`, `XDG_CURRENT_DESKTOP` and `DBUS_SESSION_BUS_ADDRESS` from the user manager's environment (`PassEnvironment`). Without `--binary`, the unit runs the `bricolo` you called.

The node reads the session from its environment ([linux.md](linux.md)), so when it starts matters:

- `--session desktop` ties the service to `graphical-session.target`: it starts once the desktop has put `DISPLAY` and the rest into the user manager, and stops with the session. GNOME and KDE Plasma do that.
- `--session headless` ties it to `default.target`: it starts at login, or at boot with lingering (`loginctl enable-linger`), with no display.
- Without `--session`, the install picks desktop when `DISPLAY` or `WAYLAND_DISPLAY` is set, and says which it chose.

A window manager started by `startx` or a display manager without systemd integration never reaches `graphical-session.target`. Add these lines to its startup file, such as `~/.xinitrc` or i3's config, and install with `--session headless`:

```sh
systemctl --user import-environment DISPLAY XAUTHORITY WAYLAND_DISPLAY
systemctl --user restart bricolo.service
```

On X11 the node gets windows, hotkeys and the clipboard. On Wayland it gets the clipboard on compositors with a data-control protocol, and the X11-only capabilities answer `unavailable` ([linux.md](linux.md)).

`--ui` also writes `~/.local/share/applications/bricolo.desktop`, so the hub appears in the applications menu, and `~/.config/autostart/bricolo.desktop`, which opens `bricolo-ui --background` at login. It needs `bricolo-ui` next to `bricolo`.

`bricolo service status` prints `systemctl --user status`. `bricolo service stop` and `start` stop and start the unit. `--dry-run` prints the files and commands.

### Uninstalling

```sh
bricolo service uninstall
rm ~/.local/bin/bricolo ~/.local/bin/bricolo-ui
```

`bricolo service uninstall` disables and stops the unit and deletes the unit and the two desktop entries. The configuration and data folders stay.

## Windows

There is no installer or release build for Windows yet, and nothing here has run on a real machine; see [windows.md](windows.md). Build `bricolo` with `cargo build --release -p bricolo-node` and put `target\release\bricolo.exe` where it will stay.

### The logon task

`bricolo service install` writes `bricolo-service.cmd` and `bricolo.xml` to `<data>\service\`, registers the task `bricolo` from the XML with `schtasks /Create /XML`, and runs it with `schtasks /Run`. `<data>` is `%LOCALAPPDATA%\bricolo` unless `BRICOLO_DATA_DIR` is set.

- The task starts at this user's logon, as the user, unelevated, with no time limit, and on battery too.
- It runs `conhost.exe --headless cmd.exe /d /s /c ""<data>\service\bricolo-service.cmd""`, which gives the node a console with no window.
- The wrapper sets `BRICOLO_CONFIG_DIR` and `BRICOLO_DATA_DIR` when they were set at install, appends the node's output to `<data>\logs\bricolo-service.log`, and starts the node again 5 seconds after it exits with an error.

`--ui` also registers `bricolo-ui`, which opens `bricolo-ui --background` at logon. `bricolo-ui` is not built for Windows yet.

`bricolo service status` prints the state lines of `schtasks /Query /V /FO LIST`. `bricolo service stop` and `start` run `schtasks /End` and `/Run`; after a stop the node starts again at the next logon. `--dry-run` prints the files and commands.

### Uninstalling

`bricolo service uninstall` ends and deletes both tasks and removes the wrappers and XML files. The configuration in `%APPDATA%\bricolo` and the data stay.

## Releases

Pushing a tag `v*` runs `.github/workflows/release.yml`, which builds and attaches to a GitHub release:

- `bricolo-<version>-macos-arm64.zip`: the app, signed ad-hoc because CI has no identity, zipped with `ditto`,
- `bricolo-<version>-linux-x86_64.tar.gz` and `-linux-aarch64.tar.gz`: `bricolo`, built in a `rust:1-bookworm` container,
- the `bricolo-ui` `.deb` and AppImage for x86_64, built on Ubuntu.

An app downloaded from a release is ad-hoc signed, so its grants do not survive an update. Build it locally with the dev identity for a Mac you use every day.
