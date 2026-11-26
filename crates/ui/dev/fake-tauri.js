// A stand-in for `window.__TAURI__`, so the hub (hub.html) and the panels
// (panel.html) run in a plain browser for design work. A classic script:
// it must run before the pages' ES modules, which read the global when
// they load. fixtures.js, loaded first, holds the data.
//
// It answers the commands bricolo-ui registers (crates/ui/src/main.rs):
// snapshot, request, start_node, hide_hub, panel_current and dismiss_panel,
// and plays the node's part for `request`, with answers shaped like
// crates/node/src/control.rs. It emits what bricolo-ui emits: node, status, ui,
// logs, log and panel.
//
// Query switches:
//   ?state=down       no node answers; Start node brings it up
//   ?state=empty      a node with nothing installed
//   ?view=<name>      open the hub on home, apps, sidecars, library, mesh or logs
//   ?recording=1      dictation holds the microphone (the Recording badge)
//   ?kind=<kind>      panel.html: caption, toast, choice (or choice-list), review
//   ?quiet=1          no live log lines or ticking widgets (steady screenshots)

(function () {
  "use strict";

  const F = window.__RH_FIXTURES__;
  if (!F) throw new Error("load dev/fixtures.js before dev/fake-tauri.js");

  const params = new URLSearchParams(location.search);
  const scenario = params.get("state") ?? "live";
  const empty = scenario === "empty";
  const quiet = params.get("quiet") === "1";
  const isPanelPage = /panel\.html$/.test(location.pathname);
  const KIND_ALIAS = { choice: "choice-list", list: "choice-list" };
  const panelKind = KIND_ALIAS[params.get("kind")] ?? params.get("kind") ?? "caption";

  // Tauri sends JSON across; copying keeps the pages off our objects.
  const copy = (v) => (v === undefined ? null : JSON.parse(JSON.stringify(v)));
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // Events

  const listeners = new Map();
  let eventId = 1;

  function listen(event, handler) {
    if (!listeners.has(event)) listeners.set(event, new Set());
    const wrapped = (payload) => handler({ event, id: eventId++, payload: copy(payload) });
    listeners.get(event).add(wrapped);
    return Promise.resolve(() => listeners.get(event)?.delete(wrapped));
  }

  function emit(event, payload) {
    for (const fn of [...(listeners.get(event) ?? [])]) {
      try {
        fn(payload);
      } catch (e) {
        console.error(`[fake-tauri] listener for "${event}" threw`, e);
      }
    }
  }

  // The node

  const node = {
    up: scenario !== "down",
    reason: "No such file or directory (os error 2)",
    pid: F.pid,
    startedAt: Date.now() - F.uptimeAtLoad * 1000,
  };

  const plugins = empty ? [] : copy(F.plugins);
  const scripts = empty ? [] : copy(F.scripts);
  const sidecars = empty ? [] : copy(F.sidecars);
  const grants = copy(F.grants);
  for (const p of plugins) {
    p.hotkeys ??= {};
    p.hotkey_defaults ??= {};
    p.overrides ??= {};
  }
  for (const s of scripts) {
    s.hotkeys ??= {};
    s.hotkey_defaults ??= {};
  }

  // What the apps hold, from which their tiles and widgets are drawn.
  const apps = {
    voice: { live: false, voice: "" },
    noise: { playing: false, color: "brown", volume: 40 },
    cities: new Set(F.citiesOn),
    dictation: { review: true, pause_media: true, recording: params.get("recording") === "1" },
    mac: { battery: 87, charging: true },
  };
  const panels = [];
  let seq = 1;

  const findPlugin = (name) => plugins.find((p) => p.name === name);
  const findScript = (name) => scripts.find((s) => s.name === name);
  const running = (name) => findPlugin(name)?.state === "running";

  // The status of one plugin or script, as serde writes it: empty hotkey
  // maps are left out, and a stopped app binds nothing.
  function appStatus(a, isPlugin) {
    const out = isPlugin
      ? { name: a.name, version: a.version, state: a.state, error: a.error, restarts: a.restarts }
      : { name: a.name, state: a.state, error: a.error, restarts: a.restarts };
    const live = a.state === "running";
    out.timers = live ? a.timers : 0;
    if (live && Object.keys(a.hotkeys).length) out.hotkeys = { ...a.hotkeys };
    if (live && Object.keys(a.hotkey_defaults).length) out.hotkey_defaults = { ...a.hotkey_defaults };
    return out;
  }

  function activeSensors() {
    return apps.dictation.recording && running("dictation") ? [{ plugin: "dictation", sensor: "mic" }] : [];
  }

  function status() {
    const now = Date.now();
    const mesh = empty
      ? { enabled: false }
      : {
          id: F.mesh.id,
          relays: F.mesh.relays,
          bound: F.mesh.bound,
          transports: F.mesh.transports,
          peers: F.mesh.peers.map(({ last_seen_ago, ...p }) => ({
            ...p,
            last_seen: Math.floor(now / 1000) - (p.state === "connected" ? 2 + Math.floor(Math.random() * 6) : last_seen_ago),
          })),
          enabled: true,
        };
    return {
      node: F.node,
      pid: node.pid,
      uptime_s: Math.floor((now - node.startedAt) / 1000),
      plugins: plugins.map((p) => appStatus(p, true)),
      sidecars: copy(sidecars),
      active_sensors: activeSensors(),
      scripts: scripts.map((s) => appStatus(s, false)),
      running: true,
      mesh,
      mcp: { enabled: true, http: "127.0.0.1:8790" },
      rules: empty ? { timeout_secs: 5, routes: {} } : F.rules,
    };
  }

  function clockRows() {
    return F.cities
      .filter(([name]) => apps.cities.has(name))
      .map(([name, zone]) => ({
        label: name,
        value: new Intl.DateTimeFormat("en-GB", { timeZone: zone, hour: "2-digit", minute: "2-digit", hour12: false }).format(new Date()),
      }));
  }

  // The node's UiSnapshot: tiles and widgets of running apps, by owner.
  function uiState() {
    const tiles = {};
    const widgets = {};
    if (running("voice-agent")) {
      const t = copy(F.tiles["voice-agent"]);
      t.switches[0].on = apps.voice.live;
      if (apps.voice.live) t.status_text = "Listening";
      t.buttons[0].label = `Voice: ${apps.voice.voice || "Automatic (by language)"}`;
      tiles["voice-agent"] = t;
      widgets["voice-agent"] = copy(F.widgets["voice-agent"]);
    }
    if (running("world-clock")) {
      tiles["world-clock"] = {
        ...copy(F.tiles["world-clock"]),
        switches: F.cities.map(([name]) => ({ id: name, label: name, on: apps.cities.has(name) })),
      };
      const rows = clockRows();
      if (rows.length) widgets["world-clock"] = { title: "World clock", rows };
    }
    if (running("mac-status")) {
      const w = copy(F.widgets["mac-status"]);
      w.rows[0].value = `${apps.mac.battery}%, ${apps.mac.charging ? "on power" : "on battery"}`;
      widgets["mac-status"] = w;
    }
    if (running("brown-noise")) {
      const n = apps.noise;
      const label = F.noiseColors.find(([id]) => id === n.color)[1];
      tiles["brown-noise"] = {
        ...copy(F.tiles["brown-noise"]),
        status_text: `${n.playing ? "Playing" : "Stopped"} · ${label} · ${n.volume}%`,
        switches: [{ id: "play", label: "Play", on: n.playing }],
        buttons: [
          { id: "down", label: "Volume −" },
          { id: "up", label: "Volume +" },
          ...F.noiseColors.map(([id, l]) => ({ id, label: `${id === n.color ? "● " : ""}${l}` })),
        ],
      };
    }
    if (running("noseguard")) {
      tiles.noseguard = {
        title: "NoseGuard",
        status_text: "Watching camera 0 · 3 touches today",
        switches: [{ id: "watch", label: "Watch", on: true }],
        buttons: [{ id: "reset", label: "Reset the count" }],
      };
    }
    if (running("dictation")) {
      const t = copy(F.tiles.dictation);
      for (const sw of t.switches) sw.on = apps.dictation[sw.id];
      if (apps.dictation.recording) t.status_text = "Recording…";
      tiles.dictation = t;
    }
    return { tiles, widgets, panels: copy(panels) };
  }

  function showPanel(owner, kind) {
    const f = F.panels[kind];
    const existing = panels.find((p) => p.owner === owner && p.kind === kind);
    const s = seq++;
    if (existing) {
      existing.seq = s;
      return existing.id;
    }
    const { owner: _, ...panel } = f;
    panels.push({ id: `p${s}`, owner, seq: s, kind, ...panel });
    return `p${s}`;
  }

  function hidePanels(owner) {
    for (let i = panels.length - 1; i >= 0; i--) if (panels[i].owner === owner) panels.splice(i, 1);
  }

  // Logs

  const logs = [];
  const KEEP = 2000;

  function sources() {
    const names = ["node"];
    for (const p of plugins) if (p.state === "running" || p.name === "noseguard") names.push(p.name);
    for (const s of scripts) if (s.state === "running") names.push(`${s.name}.lua`);
    for (const s of sidecars) names.push(`sidecar:${s.name}`);
    return names.filter((n) => F.chatter[n]);
  }

  function randomLine(ts) {
    const list = sources();
    const source = list[Math.floor(Math.random() * list.length)];
    const [level, message] = F.chatter[source][Math.floor(Math.random() * F.chatter[source].length)];
    return { ts_ms: ts, plugin: source, level, message };
  }

  function log(plugin, level, message) {
    const line = { ts_ms: Date.now(), plugin, level, message };
    logs.push(line);
    if (logs.length > KEEP) logs.shift();
    if (node.up) emit("log", line);
  }

  // The backlog: about 200 lines over the last 20 minutes, starting with
  // the node's start.
  if (!empty && node.up) {
    const now = Date.now();
    const count = 200;
    for (let i = 0; i < count; i++) {
      logs.push(randomLine(now - (count - i) * 6000 - Math.floor(Math.random() * 3000)));
    }
  } else if (empty) {
    logs.push({ ts_ms: Date.now() - 4000, plugin: "node", level: "info", message: "bricolo 0.4.0 started; data in ~/.local/share/bricolo" });
    logs.push({ ts_ms: Date.now() - 3900, plugin: "node", level: "info", message: "no plugins installed; no scripts in ~/.config/bricolo/scripts" });
  }

  // Requests

  const changed = { status: () => node.up && emit("status", status()), ui: () => node.up && emit("ui", uiState()) };

  function settingKind(def, ui) {
    const choices = (ui.choices ?? []).length > 0;
    if (typeof def === "boolean") return "bool";
    if (typeof def === "number") return Number.isInteger(def) ? "integer" : "float";
    if (typeof def === "string") {
      if (choices) return "choice";
      return /^([01][0-9]|2[0-3]):[0-5][0-9]$/.test(def) ? "time" : "text";
    }
    if (Array.isArray(def) && def.every((x) => typeof x === "string")) return choices ? "multi" : "list";
    return "other";
  }

  function advanced(def, ui) {
    if (ui.advanced !== undefined) return ui.advanced;
    return ui.label === undefined && typeof def === "string" && /^(\/|~|https?:\/\/)/.test(def);
  }

  // crates/node/src/settings.rs describe.
  function describe(p) {
    return {
      plugin: p.name,
      settings: p.settings.map(([key, def]) => {
        const ui = p.ui[key] ?? {};
        const set = key in p.overrides;
        return {
          key,
          label: ui.label ?? null,
          help: ui.help ?? null,
          kind: settingKind(def, ui),
          choices: ui.choices ?? [],
          advanced: advanced(def, ui),
          hidden: ui.hidden ?? false,
          default: def,
          value: set ? p.overrides[key] : def,
          overridden: set,
        };
      }),
    };
  }

  function checkSetting(p, key, value) {
    const entry = p.settings.find(([k]) => k === key);
    if (!entry) return `${p.name} has no setting ${JSON.stringify(key)}`;
    if (value === null) return null;
    const def = entry[1];
    const ui = p.ui[key] ?? {};
    const kind = settingKind(def, ui);
    const bad = `${key}: expected ${kind === "integer" ? "an integer" : kind === "bool" ? "true or false" : `a ${kind}`}, found ${JSON.stringify(value)}`;
    if (kind === "bool" && typeof value !== "boolean") return bad;
    if (kind === "integer" && !Number.isInteger(value)) return bad;
    if (kind === "float" && typeof value !== "number") return bad;
    if (["text", "time", "choice"].includes(kind) && typeof value !== "string") return bad;
    if (kind === "time" && !/^([01][0-9]|2[0-3]):[0-5][0-9]$/.test(value)) return `${key}: ${JSON.stringify(value)} is not a time like "18:30"`;
    if (kind === "choice" && !ui.choices.includes(value)) return `${key}: ${JSON.stringify(value)} is not one of ${ui.choices.map((c) => JSON.stringify(c)).join(", ")}`;
    if ((kind === "list" || kind === "multi") && !(Array.isArray(value) && value.every((x) => typeof x === "string"))) return bad;
    if (kind === "multi" && value.some((x) => !ui.choices.includes(x))) return `${key}: every entry must be one of ${ui.choices.join(", ")}`;
    return null;
  }

  function library() {
    return {
      os: "macos",
      holder: F.holder,
      grants: copy(grants),
      capabilities: F.capabilities.map((c) => ({
        ...c,
        plugins: plugins
          .filter((p) => p.imports.includes(c.name))
          .map((p) => p.name)
          .sort(),
      })),
      scripts: scripts.map((s) => s.name),
    };
  }

  function permissions() {
    const held = {};
    for (const g of grants) held[g.name] = g.state === "granted";
    return {
      permissions: held,
      complete: held.accessibility && held.post_events && held.input_monitoring,
    };
  }

  // Where each grant's pane is under Privacy & Security.
  const PANES = {
    accessibility: "Privacy_Accessibility",
    post_events: "Privacy_Accessibility",
    input_monitoring: "Privacy_ListenEvent",
    microphone: "Privacy_Microphone",
    camera: "Privacy_Camera",
    screen_recording: "Privacy_ScreenCapture",
  };

  // Turning a plugin on starts it again from scratch.
  function start(p) {
    p.state = "running";
    p.error = null;
    p.restarts = 0;
    if (p.name === "noseguard" && sidecars.find((s) => s.name === "noseguard")?.state !== "ready") {
      p.state = "off";
      p.restarts = 1;
      p.error = F.plugins.find((x) => x.name === "noseguard").error;
      log("noseguard", "error", p.error);
    } else {
      log(p.name, "info", `${p.name} ${p.version} started`);
    }
  }

  function stop(p) {
    if (p.name === "dictation") apps.dictation.recording = false;
    hidePanels(p.name);
  }

  function owner(req) {
    if (req.plugin && !req.script) return { kind: "plugin", item: findPlugin(req.plugin), name: req.plugin, owner: req.plugin };
    if (req.script && !req.plugin) return { kind: "script", item: findScript(req.script), name: req.script, owner: `${req.script}.lua` };
    return null;
  }

  // Effects of a switch, a button or a hotkey on the app that owns it.
  function onSwitch(who, id, on) {
    if (who === "voice-agent") {
      apps.voice.live = on;
      log(who, "info", on ? "call started" : "call ended");
    } else if (who === "world-clock") {
      if (on) apps.cities.add(id);
      else apps.cities.delete(id);
      log(who, "debug", `drew ${apps.cities.size} cities`);
    } else if (who === "brown-noise") {
      apps.noise.playing = on;
      log(who, "info", on ? `playing noise_${apps.noise.color}.wav` : "stopped");
    } else if (who === "dictation") {
      apps.dictation[id] = on;
    }
  }

  function onButton(who, id) {
    if (who === "brown-noise") {
      const n = apps.noise;
      if (id === "up") n.volume = Math.min(100, n.volume + 10);
      else if (id === "down") n.volume = Math.max(0, n.volume - 10);
      else n.color = id;
      log(who, "debug", id === "up" || id === "down" ? `volume ${n.volume}%` : `color ${id}`);
    } else if (who === "voice-agent" && id === "voice") {
      showPanel(who, "choice-list");
      log(who, "debug", "showing 11 voices");
    } else if (who === "voice-agent" && id === "restart") {
      log(who, "warn", "restarting the daemon on 127.0.0.1:8792");
    } else if (who === "dictation" && id === "rewrite") {
      showPanel(who, "choice-list");
    } else {
      log(who, "info", `button ${id}`);
    }
  }

  function toggleRecording() {
    const d = apps.dictation;
    d.recording = !d.recording;
    if (d.recording) {
      showPanel("dictation", "caption");
      log("dictation", "info", "recording (chord)");
    } else {
      hidePanels("dictation");
      log("dictation", "info", "take of 4.8 s transcribed in 0.38 s by parakeet");
      if (d.review) showPanel("dictation", "review");
    }
  }

  async function handle(req) {
    const cmd = req?.cmd;
    switch (cmd) {
      case "status":
        return status();

      case "reload": {
        const p = findPlugin(req.plugin);
        if (!p) return { error: `no plugin named ${req.plugin} is installed` };
        if (p.state === "disabled") return { error: `${p.name} is turned off; turn it on first` };
        stop(p);
        start(p);
        changed.status();
        changed.ui();
        return { reloaded: p.name, status: appStatus(p, true) };
      }

      case "reload-script": {
        const s = findScript(req.script);
        if (!s) return { error: `no script named ${req.script}` };
        s.state = "running";
        s.error = null;
        s.restarts = 0;
        log(`${s.name}.lua`, "info", `${s.name}.lua reloaded`);
        changed.status();
        return { reloaded: s.name, status: appStatus(s, false) };
      }

      case "enable":
      case "disable": {
        const o = owner(req);
        if (!o) return { error: "name either a plugin or a script" };
        if (!o.item) return { error: `no ${o.kind} named ${o.name}` };
        const on = cmd === "enable";
        if (on && o.item.state === "disabled") {
          if (o.kind === "plugin") start(o.item);
          else o.item.state = "running";
        } else if (!on && o.item.state !== "disabled") {
          if (o.kind === "plugin") stop(o.item);
          o.item.state = "disabled";
          o.item.error = null;
          log("node", "info", `${o.owner} turned off in the hub`);
        }
        changed.status();
        changed.ui();
        return { enabled: on, status: appStatus(o.item, o.kind === "plugin") };
      }

      case "restart-sidecar": {
        const s = sidecars.find((x) => x.name === req.sidecar);
        if (!s) return { error: `no sidecar named ${req.sidecar} in node.toml` };
        s.state = "starting";
        s.pid = null;
        log(`sidecar:${s.name}`, "info", `restarting ${s.name} on port ${s.port}`);
        setTimeout(() => {
          s.state = "ready";
          s.pid = 48000 + Math.floor(Math.random() * 2000);
          log(`sidecar:${s.name}`, "info", `ready on 127.0.0.1:${s.port}`);
          changed.status();
        }, 1500);
        changed.status();
        return { restarting: s.name };
      }

      case "ui-action": {
        const a = req.action ?? {};
        const ui = uiState();
        if (a.kind === "switch") {
          const sw = ui.tiles[a.owner]?.switches.find((w) => w.id === a.id);
          if (!sw) return { error: `${a.owner} has no switch ${a.id}` };
          onSwitch(a.owner, a.id, a.on);
        } else if (a.kind === "button") {
          if (!ui.tiles[a.owner]?.buttons.some((b) => b.id === a.id)) return { error: `${a.owner} has no button ${a.id}` };
          onButton(a.owner, a.id);
        } else if (a.kind === "choice" || a.kind === "review") {
          const kind = a.kind === "choice" ? "choice-list" : "review";
          const i = panels.findIndex((p) => p.id === a.panel && p.kind === kind);
          if (i < 0) return { error: `no ${a.kind === "choice" ? "ChoiceList" : "Review"} panel ${a.panel}` };
          const [p] = panels.splice(i, 1);
          if (a.kind === "choice" && p.owner === "voice-agent" && a.index != null) {
            apps.voice.voice = p.choices[a.index].replace(/^✓ /, "").replace("Automatic (by language)", "");
          }
          log(p.owner, "debug", a.kind === "choice" ? `choice ${a.index}` : `review ${a.accepted ? "accepted" : "cancelled"}`);
        } else {
          return { error: `bad request: unknown variant \`${a.kind}\`` };
        }
        changed.ui();
        return { ok: true };
      }

      case "fire": {
        const p = findPlugin(req.plugin);
        if (!p || p.state !== "running") return { error: `no running plugin named ${req.plugin}` };
        if (!(req.hotkey in p.hotkeys)) return { error: `${p.name} has no hotkey ${req.hotkey}` };
        if (p.name === "dictation" && req.hotkey === "h1") toggleRecording();
        else if (p.name === "voice-agent") onSwitch("voice-agent", "call", !apps.voice.live);
        else log(p.name, "info", `hotkey ${req.hotkey}`);
        changed.status();
        changed.ui();
        return { fired: req.hotkey, plugin: p.name };
      }

      case "set-hotkey": {
        const o = owner(req);
        if (!o) return { error: "name either a plugin or a script" };
        if (!o.item || o.item.state !== "running") return { error: `no running ${o.kind} named ${o.name}` };
        const { hotkeys, hotkey_defaults: defaults } = o.item;
        if (!(req.id in hotkeys)) return { error: `${o.owner} has no hotkey ${req.id}` };
        const chord = req.chord ?? defaults[req.id] ?? hotkeys[req.id];
        if (chord === "cmd+space" || chord === "cmd+tab") return { error: `cannot bind ${chord}: another app holds it` };
        for (const other of [...plugins, ...scripts]) {
          if (other.state !== "running") continue;
          for (const [id, c] of Object.entries(other.hotkeys)) {
            if (c === chord && !(other === o.item && id === req.id)) {
              const who = scripts.includes(other) ? `${other.name}.lua` : other.name;
              return { error: `${chord} is already bound by ${who}` };
            }
          }
        }
        defaults[req.id] ??= hotkeys[req.id];
        hotkeys[req.id] = chord;
        if (defaults[req.id] === chord) delete defaults[req.id];
        log("node", "info", `${o.owner}: ${req.id} bound to ${chord}`);
        changed.status();
        return { hotkey: req.id, owner: o.owner, chord };
      }

      case "settings": {
        const p = findPlugin(req.plugin);
        if (!p) return { error: `no plugin named ${req.plugin} is installed` };
        return describe(p);
      }

      case "set-settings": {
        const p = findPlugin(req.plugin);
        if (!p) return { error: `no plugin named ${req.plugin} is installed` };
        const values = req.values ?? {};
        for (const [key, value] of Object.entries(values)) {
          const problem = checkSetting(p, key, value);
          if (problem) return { error: problem };
        }
        for (const [key, value] of Object.entries(values)) {
          const def = p.settings.find(([k]) => k === key)[1];
          if (value === null || JSON.stringify(value) === JSON.stringify(def)) delete p.overrides[key];
          else p.overrides[key] = value;
        }
        await sleep(300);
        if (p.state !== "disabled") {
          stop(p);
          start(p);
        }
        log("node", "info", `wrote [plugins.${p.name}] in node.toml and reloaded ${p.name}`);
        changed.status();
        changed.ui();
        return { ...describe(p), status: appStatus(p, true) };
      }

      case "library":
        return library();

      case "permissions": {
        if (req.grant !== undefined) {
          const g = grants.find((x) => x.name === req.grant);
          if (!g) return { error: `no grant named ${req.grant}` };
          // macOS asks once: "not asked" turns into an answer, here Allow.
          if (req.request && g.state === "not-asked") g.state = "granted";
        }
        return permissions();
      }

      case "restart-node":
        // The node starts again and sees what macOS gave it meanwhile.
        for (const g of grants) if (g.state === "restart") g.state = "granted";
        return { restarting: true };

      case "open-settings": {
        const g = grants.find((x) => x.name === req.grant);
        if (!g) return { error: `no grant named ${req.grant}; use one of ${grants.map((x) => x.name).join(", ")}` };
        // As if the user turned it on in System Settings a moment later.
        if (g.state !== "granted") {
          setTimeout(() => {
            g.state = "granted";
            log("node", "info", `${g.label} granted`);
          }, 3000);
        }
        return { opened: `x-apple.systempreferences:com.apple.preference.security?${PANES[g.name]}`, grant: g.name };
      }

      case "mcp-connect": {
        const where = req.client === "claude-desktop" ? "Claude Desktop" : "Claude Code";
        log("node", "info", `added bricolo to ${where}`);
        return { connected: true, message: `Added bricolo to ${where}. Restart it to see it.` };
      }

      default:
        return { error: `bad request: unknown variant \`${cmd}\`` };
    }
  }

  // The commands bricolo-ui registers

  // crates/ui/src/client.rs ALLOWED: what a page may send.
  const ALLOWED = new Set(["status", "reload", "reload-script", "enable", "disable", "restart-sidecar", "ui-action", "fire", "set-hotkey", "library", "permissions", "open-settings", "restart-node", "settings", "set-settings", "mcp-connect"]);

  function panelFor(kind) {
    const f = F.panels[kind];
    if (!f) return null;
    const { owner, ...panel } = f;
    return { id: `p${kind.length}`, owner, seq: seq++, kind, ...panel };
  }

  const commands = {
    snapshot: () => ({
      connected: node.up,
      reason: node.up ? "" : node.reason,
      status: node.up ? status() : null,
      ui: node.up ? uiState() : null,
      logs: node.up ? copy(logs) : [],
      can_start: true,
      socket: "~/.local/share/bricolo/bricolo.sock",
      view: params.get("view"),
    }),

    request: async ({ request: req }) => {
      if (!ALLOWED.has(req?.cmd)) throw `the hub does not send ${JSON.stringify(req?.cmd ?? null)}`;
      if (isPanelPage) {
        console.info("[fake-tauri] panel sent", req);
        return { ok: true };
      }
      if (!node.up) throw node.reason;
      await sleep(40);
      return copy(await handle(copy(req)));
    },

    start_node: () => {
      if (node.up) return node.pid;
      node.pid = F.pid;
      node.startedAt = Date.now();
      setTimeout(() => {
        node.up = true;
        log("node", "info", "bricolo 0.4.0 started; data in ~/.local/share/bricolo");
        emit("node", { connected: true });
        emit("status", status());
        emit("ui", uiState());
        emit("logs", logs);
      }, 1200);
      return node.pid;
    },

    hide_hub: () => console.info("[fake-tauri] hide_hub: bricolo-ui would hide the hub now"),

    panel_current: ({ kind }) => panelFor(kind),

    // bricolo-ui hides the panel; the node then clears it. Here it comes back
    // after a moment, so the page stays useful.
    dismiss_panel: ({ kind }) => {
      emit("panel", { kind, panel: null });
      setTimeout(() => emit("panel", { kind, panel: panelFor(kind) }), 1500);
    },
  };

  async function invoke(cmd, args = {}) {
    const fn = commands[cmd];
    if (!fn) throw `command ${cmd} not found`;
    return copy(await fn(copy(args)));
  }

  window.__TAURI__ = {
    core: { invoke },
    event: { listen, emit: (event, payload) => Promise.resolve(emit(event, payload)) },
    window: { getCurrentWindow: () => ({ label: isPanelPage ? `panel-${panelKind}` : "hub" }) },
  };

  // For poking at it from the console: __rhFake.log("node", "warn", "x").
  window.__rhFake = { emit, log, status, uiState, handle, state: { plugins, scripts, sidecars, grants, apps, panels } };

  // Panel windows have the size bricolo-ui gives them (crates/ui/src/placement.rs).
  if (isPanelPage) {
    const SIZES = { caption: [760, 120], toast: [420, 72], "choice-list": [440, 360], review: [760, 200] };
    const [w, h] = SIZES[panelKind] ?? [600, 340];
    document.documentElement.style.setProperty("--dev-w", `${w}px`);
    document.documentElement.style.setProperty("--dev-h", `${h}px`);
    return;
  }

  // Ticks: a log line every second or two, widgets that move.

  if (quiet) return;
  let lastUi = "";
  function tick(fn, ms) {
    setInterval(() => {
      if (node.up) fn();
    }, ms);
  }
  (function nextLine() {
    setTimeout(() => {
      if (node.up) {
        const line = randomLine(Date.now());
        log(line.plugin, line.level, line.message);
      }
      nextLine();
    }, 1000 + Math.random() * 1000);
  })();
  tick(() => {
    if (Math.random() < 0.5) apps.mac.battery = Math.max(5, Math.min(100, apps.mac.battery + (apps.mac.charging ? 1 : -1)));
    const ui = JSON.stringify(uiState());
    if (ui !== lastUi) {
      lastUi = ui;
      emit("ui", JSON.parse(ui));
    }
  }, 10000);
  // The connected peer's last_seen moves, so the node reports a change.
  tick(() => changed.status(), 5000);
})();
