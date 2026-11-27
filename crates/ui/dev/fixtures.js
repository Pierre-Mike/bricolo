// Fixture data for the hub preview: a node running the apps in apps/ and
// examples/apps. A classic script, loaded before fake-tauri.js, which owns
// the behaviour. The shapes follow the node's answers:
//   status   crates/runtime/src/runtime.rs Status, crates/lua/src/tier.rs
//            ScriptStatus, crates/providers/src/sidecar.rs SidecarStatus,
//            crates/mesh/src/mesh.rs MeshStatus, and control.rs Node::status
//   ui       crates/providers/src/ui.rs UiSnapshot
//   logs     crates/providers/src/log.rs LogLine
//   library  crates/node/src/library.rs answer, crates/providers/src/catalog.rs
//   settings crates/node/src/settings.rs describe
// Change these when the Rust shapes change.

(function () {
  "use strict";

  // Plugins, from apps/<name>/plugin/plugin.toml. `settings` keeps the
  // plugin.toml order; `ui` is [settings_ui.<key>].
  const plugins = [
    {
      name: "voice-agent",
      version: "0.1.0",
      state: "running",
      error: null,
      restarts: 0,
      timers: 2,
      hotkeys: { h1: "ctrl+alt+cmd+v" },
      imports: ["log", "kv", "timer", "hotkey", "ui", "http", "shell", "disk"],
      settings: [
        ["chord", "ctrl+alt+cmd+v"],
        ["project", "/Users/me/Github/pipecat-voice-agent"],
        ["uv", "/Users/me/.local/bin/uv"],
        ["user", "me"],
        ["base_url", "http://127.0.0.1:8792"],
        ["pocket_tts_url", "http://127.0.0.1:8791"],
        ["daemon_log", "/tmp/hs-voice-agent-daemon.log"],
        ["retro_log", "/tmp/hs-voice-agent-retro.log"],
        ["retro_at", "18:30"],
        ["startup_grace_s", 90],
        ["relaunches", 3],
        ["speed_turns", 50],
      ],
      ui: {
        chord: { hidden: true },
        retro_at: { label: "Daily review time" },
        speed_turns: { label: "Turns in the speed widget" },
        startup_grace_s: { label: "Daemon start grace (s)", help: "How long a starting daemon may take before it counts as failed." },
        relaunches: { label: "Daemon relaunches" },
        user: { advanced: true },
      },
      // Set in node.toml under [plugins.voice-agent].
      overrides: { retro_at: "19:00" },
    },
    {
      name: "world-clock",
      version: "0.1.0",
      state: "running",
      error: null,
      restarts: 0,
      timers: 1,
      imports: ["log", "kv", "timer", "ui", "shell"],
      settings: [["every_ms", 30000]],
      ui: {},
    },
    {
      name: "mac-status",
      version: "0.1.0",
      state: "running",
      error: null,
      restarts: 0,
      timers: 1,
      imports: ["log", "timer", "ui", "shell", "system"],
      settings: [["every_ms", 10000]],
      ui: {},
    },
    {
      name: "claude-usage",
      version: "0.1.0",
      state: "running",
      error: null,
      restarts: 0,
      timers: 1,
      imports: ["log", "timer", "ui", "shell", "http"],
      settings: [["every_ms", 120000]],
      ui: {},
    },
    {
      name: "brown-noise",
      version: "0.1.0",
      state: "running",
      error: null,
      restarts: 0,
      timers: 0,
      imports: ["log", "kv", "ui", "audio", "disk"],
      settings: [
        ["sounds", "~/.config/bricolo/sounds/noise"],
        ["step", 10],
      ],
      ui: {},
    },
    {
      name: "dictation",
      version: "0.1.0",
      state: "running",
      error: null,
      restarts: 0,
      timers: 1,
      // The user moved the chord in the hub: hotkeys.toml holds it.
      hotkeys: { h1: "cmd+shift+d", h2: "fn" },
      hotkey_defaults: { h1: "ctrl+alt+shift+d" },
      imports: ["log", "kv", "timer", "hotkey", "mic", "speech", "clipboard", "keyboard", "ui", "shell", "disk", "audio"],
      settings: [
        ["chord", "ctrl+alt+shift+d"],
        ["hold", "fn"],
        ["min_hold_ms", 300],
        ["max_secs", 120],
        ["prime_mic", true],
        ["media_control", "/opt/homebrew/bin/media-control"],
        ["presets_file", "~/.config/bricolo/text_actions.json"],
        ["claude", "/Users/me/.local/bin/claude"],
        ["start_sound", "/System/Library/Sounds/Tink.aiff"],
      ],
      ui: {
        chord: { hidden: true },
        hold: { label: "Hold to record", help: "The key that records while held; empty turns holding off." },
        min_hold_ms: { label: "Shortest hold (ms)", help: "A shorter hold is a slip, and is dropped." },
        max_secs: { label: "Longest take (s)" },
        prime_mic: { label: "Wake the microphone at start" },
        start_sound: {
          label: "Start sound",
          choices: ["/System/Library/Sounds/Tink.aiff", "/System/Library/Sounds/Pop.aiff", "/System/Library/Sounds/Glass.aiff", ""],
          help: "Empty turns it off.",
        },
        media_control: { help: "Must stay in [shell] allow of plugin.toml." },
        claude: { help: "Must stay in [shell] allow of plugin.toml." },
        presets_file: { help: "Must stay covered by the disk permissions of plugin.toml." },
      },
      overrides: { max_secs: 180 },
    },
    {
      // Off: init failed twice, because its sidecar does not answer.
      name: "noseguard",
      version: "0.1.0",
      state: "off",
      error: "init failed: GET http://127.0.0.1:8802/status: timed out after 2000 ms",
      restarts: 1,
      timers: 0,
      imports: ["log", "kv", "timer", "ui", "http"],
      settings: [
        ["url", "http://127.0.0.1:8802"],
        ["poll_ms", 2000],
        ["timeout_ms", 5000],
        ["status_timeout_ms", 2000],
        ["cameras_timeout_ms", 30000],
      ],
      ui: {},
    },
    {
      // Turned off in the hub: state.toml lists it.
      name: "tailscale-funnel",
      version: "0.1.0",
      state: "disabled",
      error: null,
      restarts: 0,
      timers: 0,
      imports: ["log", "timer", "clipboard", "app", "http", "ui"],
      settings: [
        ["url", "http://127.0.0.1:8795"],
        ["poll_ms", 30000],
        ["timeout_ms", 30000],
      ],
      ui: {},
    },
  ];

  const scripts = [
    {
      // Running the old copy: the last save was refused.
      name: "live_check",
      state: "running",
      error: "reload refused: scripts/live_check.lua:142: 'end' expected (to close 'function' at line 120) near <eof>",
      restarts: 0,
      timers: 0,
      hotkeys: { h1: "ctrl+alt+cmd+shift+l", h2: "ctrl+alt+cmd+shift+s", h3: "ctrl+alt+cmd+shift+c" },
    },
    {
      name: "center_window",
      state: "running",
      error: null,
      restarts: 0,
      timers: 0,
      hotkeys: { h1: "ctrl+alt+c" },
    },
  ];

  const sidecars = [
    { name: "parakeet", state: "ready", port: 8765, pid: 48211, restarts: 0, error: null },
    { name: "pocket-en", state: "ready", port: 8791, pid: 48230, restarts: 1, error: "exited with status 1 (see sidecar:pocket-en in the logs)" },
    { name: "noseguard", state: "unhealthy", port: 8802, pid: 48244, restarts: 0, error: "GET /health: timed out after 2 s" },
  ];

  const mesh = {
    id: "8f3c1e9a5b7d2046c1e8a9f03b6d4e2c7a1f5b9d3e8c6a40f2b7d1e9c3a5f8b6",
    relays: true,
    bound: ["0.0.0.0:51843", "[::]:51844", "100.101.1.1:7787"],
    transports: [
      { kind: "iroh", bound: ["0.0.0.0:51843", "[::]:51844"], detail: "n0 relays and DNS discovery" },
      { kind: "tailscale", bound: ["100.101.1.1:7787"], detail: "peers from /usr/bin/tailscale status" },
    ],
    peers: [
      {
        name: "studio-mini",
        id: "c47a0e3b9d12f8e6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2",
        allow: ["speech", "tts", "notify"],
        state: "connected",
        via: "tailscale",
        incoming: 1,
        last_seen_ago: 4,
        last_error: null,
      },
      {
        name: "linux-box",
        id: "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f809",
        allow: ["speech"],
        state: "offline",
        via: null,
        incoming: 0,
        last_seen_ago: 3 * 3600 + 720,
        last_error: "timed out connecting after 10 s",
      },
    ],
  };

  const rules = { timeout_secs: 5, routes: { speech: ["studio-mini", "local"], tts: ["local"] } };

  // Tiles and widgets the apps set, by owner (scripts as <name>.lua).
  const tiles = {
    "voice-agent": {
      title: "Voice agent",
      status_text: "Ready · 2 jobs open, 1 needs you",
      switches: [{ id: "call", label: "Call", on: false }],
      buttons: [
        { id: "voice", label: "Voice: Automatic (by language)" },
        { id: "review", label: "Run the daily review" },
        { id: "open", label: "Open the latest review" },
        { id: "restart", label: "Restart the daemon" },
      ],
    },
    "brown-noise": {
      title: "Noise",
      status_text: "",
      switches: [{ id: "play", label: "Play", on: false }],
      buttons: [],
    },
    "world-clock": { title: "Cities", status_text: "", switches: [], buttons: [] },
    dictation: {
      title: "Dictation",
      status_text: "Hold Fn to talk, or ctrl+alt+shift+d to start and stop",
      switches: [
        { id: "review", label: "Review before pasting", on: true },
        { id: "pause_media", label: "Pause music while recording", on: true },
      ],
      buttons: [
        { id: "rewrite", label: "Rewrite the last take…" },
        { id: "copy", label: "Copy the last take" },
      ],
    },
  };

  // From apps/world-clock/plugin/src/domain.rs, west to east.
  const cities = [
    ["Honolulu", "Pacific/Honolulu"],
    ["Seattle", "America/Los_Angeles"],
    ["Denver", "America/Denver"],
    ["Chicago", "America/Chicago"],
    ["New York", "America/New_York"],
    ["São Paulo", "America/Sao_Paulo"],
    ["London", "Europe/London"],
    ["Paris", "Europe/Paris"],
    ["Dubai", "Asia/Dubai"],
    ["Mumbai", "Asia/Kolkata"],
    ["Singapore", "Asia/Singapore"],
    ["Tokyo", "Asia/Tokyo"],
    ["Sydney", "Australia/Sydney"],
  ];
  const citiesOn = ["Seattle", "New York", "Paris"];

  const noiseColors = [
    ["brown", "Brown"],
    ["pink", "Pink"],
    ["white", "White"],
    ["blue", "Blue"],
    ["violet", "Violet"],
  ];

  const widgets = {
    "voice-agent": {
      title: "Voice agent speed",
      rows: [
        { label: "Turns", value: "50 of 214 today" },
        { label: "Reply starts", value: "1.42 s median" },
        { label: "Hearing you", value: "0.31 s" },
        { label: "Thinking", value: "0.64 s" },
        { label: "First audio", value: "0.47 s" },
        { label: "Slowest", value: "3.9 s, 14:02" },
      ],
    },
    "mac-status": {
      title: "This Mac",
      rows: [
        { label: "Battery", value: "87%, on power" },
        { label: "Memory", value: "62%, 20 of 32 GB" },
        { label: "Disk", value: "187 GB free" },
      ],
    },
    "claude-usage": {
      title: "Claude usage",
      gauges: [
        { label: "Session", value: "5%", fill: 5, marks: 5, now: 44, note: "Resets in 2 h 48 min" },
        { label: "Week", value: "48%", fill: 48, marks: 7, now: 41, note: "Resets in 4 d 3 h" },
      ],
      rows: [],
    },
  };

  // Panels on screen. The hub does not draw them (bricolo-ui shows each in its
  // own window); panel.html picks one by kind.
  const panels = {
    caption: { owner: "dictation", title: "Listening", text: "so the plan for tomorrow is to move the standup to ten and", choices: [], timeout_ms: null },
    toast: { owner: "live_check.lua", title: "", text: "live_check: 41 PASS, 0 FAIL, 3 UNAVAILABLE", choices: [], timeout_ms: 8000 },
    "choice-list": {
      owner: "voice-agent",
      title: "Voice of the agent",
      text: "",
      choices: ["✓ Automatic (by language)", "Alba", "Marius", "Peter Yearsley", "Javert", "Jean", "Fantine", "Cosette", "Eponine", "Azelma", "Anna"],
      timeout_ms: null,
    },
    review: {
      owner: "dictation",
      title: "Review before pasting",
      text: "So the plan for tomorrow is to move the standup to ten, then review the hub mockups with the team before lunch.",
      choices: [],
      timeout_ms: null,
    },
  };

  // The grants, as permissions::GRANTS lists them, and their state.
  const grants = [
    { name: "accessibility", label: "Accessibility", state: "granted" },
    { name: "post_events", label: "Post key and mouse events", state: "granted" },
    { name: "input_monitoring", label: "Input Monitoring", state: "missing" },
    { name: "microphone", label: "Microphone", state: "granted" },
    { name: "camera", label: "Camera", state: "not-asked" },
    // Turned on while the node ran: it needs a restart to see it.
    { name: "screen_recording", label: "Screen Recording", state: "restart" },
  ];

  // catalog.rs ENTRIES: name, needs, note; summary is the first sentence
  // of the doc comment above `interface` in wit/<name>.wit.
  const capabilities = [
    ["log", "Plugin logging.", null, null],
    ["timer", "Timers.", null, null],
    ["hotkey", "Global keyboard shortcuts.", "input_monitoring", "only the fn key"],
    ["tts", "Text to speech.", null, null],
    ["speech", "Speech to text.", null, null],
    ["disk", "Files outside the plugin's own data folder, limited to the paths granted at install (disk:~/Notes, disk:read:~/Documents).", null, null],
    ["clipboard", "The system clipboard, plain text only.", null, null],
    ["kv", "A small key/value store private to each plugin, kept in its data folder.", null, null],
    ["window", "Windows and screens of this device.", "accessibility", null],
    ["app", "Apps on this device.", null, null],
    ["notify", "Notifications shown to the user.", null, null],
    ["shell", "Runs programs listed under [shell] allow in plugin.toml.", null, null],
    ["http", "HTTP requests to the host:port pairs listed under [http] allow in plugin.toml.", null, null],
    ["ui", "A tile of controls in the plugin's hub card, a display-only widget, and a few kinds of floating panel, described as data.", null, null],
    ["keyboard", "Synthetic key presses on this device: shortcuts and typed text.", "post_events", null],
    ["mic", "Records the microphone of this device into the caller's data folder, through ffmpeg.", "microphone", null],
    ["audio", "Plays sound files on this device's default output.", null, null],
    ["system", "The state of this device.", null, null],
    ["files", "Watches folders for changes.", null, null],
    ["mouse", "The mouse pointer of this device.", "post_events", "moving and clicking"],
    ["screen", "Pictures of this device's screens.", "screen_recording", null],
    ["selection", "The text selected in the frontmost app.", "accessibility", null],
    ["camera", "Photos from this device's camera, taken with ffmpeg like mic.", "camera", null],
    ["secrets", "Small secrets such as API keys, kept in the macOS Keychain.", null, null],
    ["claude-code", "Claude Code sessions on this node: start one on a task in a folder, follow it, send it more work, stop it.", null, null],
    ["accessibility", "The accessibility tree of an app's windows: read it, find controls, and click, type into, pick menu items in, or act on them, in the background where the system allows.", "accessibility", "screenshots also need Screen Recording", "screen_recording"],
    ["process", "Programs running on this device, with their CPU and memory use, and stopping or killing one.", null, null],
    ["device", "The volume, brightness, Wi-Fi, appearance, Focus, lock and sleep of this device.", null, null],
    ["scripting", "Runs AppleScript, JavaScript for Automation and Shortcuts on macOS, and PowerShell on Windows (and wherever pwsh is installed).", null, "each app a script drives asks for Automation access once"],
    ["virtual-screen", "Screens that no monitor shows, where apps run and are driven without touching the user's screen, keyboard or pointer.", null, null],
  ].map(([name, summary, needs, note, also = null]) => ({ name, summary, needs, note, also }));

  // What each source says in the logs: [level, message]. Picked at random
  // for the backlog and for the live lines.
  const chatter = {
    node: [
      ["info", "bricolo 0.4.0 started; data in ~/.local/share/bricolo"],
      ["info", "control socket listening on ~/.local/share/bricolo/bricolo.sock"],
      ["debug", "status changed; 1 subscriber"],
      ["debug", "hub subscribed"],
      ["info", "reloaded rules.toml: speech → studio-mini, local"],
      ["trace", "event queue voice-agent: 0 pending"],
      ["warn", "hotkey ctrl+alt+shift+d asked by dictation is bound as cmd+shift+d (hotkeys.toml)"],
    ],
    "voice-agent": [
      ["info", "daemon is up on 127.0.0.1:8792"],
      ["debug", "poll: idle, 2 jobs open, 1 needs you"],
      ["debug", "speed widget: 50 turns from latency-2026-11-22.jsonl"],
      ["info", "voice set to automatic"],
      ["warn", "GET /status: 503 while the daemon loads its model"],
    ],
    "world-clock": [
      ["debug", "drew 3 cities"],
      ["trace", "date TZ=Europe/Paris +%H:%M"],
    ],
    "mac-status": [
      ["debug", "battery 87%, on power"],
      ["trace", "vm_stat: 1.2 ms"],
      ["debug", "disk 187 GB free"],
    ],
    "brown-noise": [
      ["info", "loaded 5 noise files"],
      ["debug", "volume 40%"],
    ],
    dictation: [
      ["info", "recording (fn held)"],
      ["info", "take of 6.2 s transcribed in 0.41 s by parakeet"],
      ["debug", "music paused by media-control"],
      ["info", "pasted 128 characters into Slack"],
      ["warn", "take shorter than 300 ms dropped"],
      ["debug", "review accepted"],
    ],
    noseguard: [
      ["error", "init failed: GET http://127.0.0.1:8802/status: timed out after 2000 ms"],
      ["warn", "restarting once after a failure"],
    ],
    "live_check.lua": [
      ["info", "live_check: PASS clipboard.set/get"],
      ["info", "live_check: PASS window.focused"],
      ["info", "live_check: UNAVAILABLE camera.snap"],
      ["error", "reload refused: scripts/live_check.lua:142: 'end' expected (to close 'function' at line 120) near <eof>"],
    ],
    "center_window.lua": [
      ["info", "centered Safari"],
      ["warn", "no focused window"],
    ],
    "sidecar:parakeet": [
      ["info", "INFO:     127.0.0.1:52144 - \"POST /transcribe HTTP/1.1\" 200 OK"],
      ["debug", "INFO:     127.0.0.1:52101 - \"GET /health HTTP/1.1\" 200 OK"],
    ],
    "sidecar:pocket-en": [
      ["info", "INFO:     127.0.0.1:52210 - \"POST /speak HTTP/1.1\" 200 OK"],
      ["debug", "INFO:     127.0.0.1:52101 - \"GET /health HTTP/1.1\" 200 OK"],
    ],
    "sidecar:noseguard": [
      ["warn", "health check failed: GET /health timed out after 2 s"],
      ["info", "camera 0: FaceTime HD Camera, 1280x720"],
      ["error", "mediapipe: no frames from camera 0 for 5 s"],
    ],
  };

  window.__RH_FIXTURES__ = {
    node: "macbook",
    pid: 48190,
    uptimeAtLoad: 3 * 3600 + 17 * 60 + 42,
    plugins,
    scripts,
    sidecars,
    mesh,
    rules,
    tiles,
    widgets,
    cities,
    citiesOn,
    noiseColors,
    panels,
    grants,
    capabilities,
    chatter,
    holder: "bricolo",
  };
})();
