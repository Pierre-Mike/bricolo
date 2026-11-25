// The hub: Home, Apps (plugins and scripts, each with a drawer), and the
// System views: sidecars, the library, mesh and logs. It draws what bricolo-ui
// last heard from the node and sends the user's actions back as control
// requests.

import { closeApp, openApp, renderApps, renderDrawer } from "./apps.js";
import { isRecording, setupHotkeys } from "./chord.js";
import { badge, emptyState, h } from "./dom.js";
import { problemCount, renderHome } from "./home.js";
import { icon } from "./icons.js";
import { refresh as refreshLibrary, renderLibrary, setupLibrary, update as updateLibrary } from "./library.js";
import { isPaletteOpen, openPalette } from "./palette.js";
import { actions, apps, model, view } from "./store.js";
import { invoke, listen, request } from "./tauri.js";

const VIEWS = {
  home: { title: "Home", icon: "home", render: renderHome },
  apps: { title: "Apps", icon: "apps", render: renderApps },
  sidecars: { title: "Sidecars", icon: "server", render: renderSidecars, group: "System" },
  library: { title: "Library", icon: "library", render: renderLibrary },
  mesh: { title: "Mesh and routes", icon: "network", render: renderMesh },
  logs: { title: "Logs", icon: "terminal", render: renderLogs },
};

// Names `--view` took before Home and Apps.
const OLD_VIEWS = { widgets: "home", plugins: "apps", scripts: "apps" };

// Log filters survive re-renders.
const filters = { source: "", level: "info", text: "", follow: true };
const LEVELS = ["trace", "debug", "info", "warn", "error"];
const MAX_LOG_ROWS = 2000;

const $ = (id) => document.getElementById(id);

// Requests

async function send(req) {
  try {
    const answer = await request(req);
    if (answer && answer.error) report(answer.error);
    return answer;
  } catch (e) {
    report(String(e));
  }
}

function report(message) {
  model.logs.push({ ts_ms: Date.now(), plugin: "bricolo-ui", level: "error", message });
  if (view.current === "logs") appendLog(model.logs[model.logs.length - 1]);
  toast(message, "error");
  render();
}

// Errors come back in the answer, for callers that show them in place.
async function quietly(req) {
  try {
    return await request(req);
  } catch (e) {
    return { error: String(e) };
  }
}

// A short note at the bottom of the hub. Errors stay longer, carry an
// icon and a way to the logs; anything else fades after 2.5 s. Callers that
// pass no tone get "info"; report() passes "error".
let toastTimer;
function toast(text, tone = "info") {
  const el = $("toast");
  const error = tone === "error";
  el.className = `toast ${tone}`;
  el.replaceChildren(
    icon(error ? "error" : "check", { size: 15 }),
    h("span", { class: "toast-text" }, text),
    error ? h("button", { class: "toast-action", onclick: () => { el.hidden = true; showLogs(""); } }, "Logs") : null,
  );
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), error ? 6000 : 2500);
}

function go(key) {
  view.current = key in VIEWS ? key : (OLD_VIEWS[key] ?? "home");
  view.open = null;
  render();
}

function showLogs(source) {
  filters.source = source;
  // The logs view keeps itself across renders; this makes it redraw with
  // the new filter even when it already shows.
  $("view").dataset.view = "";
  go("logs");
}

async function startNode(button) {
  button.disabled = true;
  button.textContent = "Starting…";
  try {
    await invoke("start_node");
  } catch (e) {
    report(String(e));
    button.disabled = false;
    button.textContent = "Start node";
  }
}

// Formatting

function duration(seconds) {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const hours = Math.floor(m / 60);
  if (hours < 48) return `${hours}h ${m % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

function clock(ms) {
  if (!ms) return "";
  const d = new Date(ms);
  return d.toTimeString().slice(0, 8);
}

function ago(unixSeconds) {
  if (!unixSeconds) return "never";
  return `${duration(Date.now() / 1000 - unixSeconds)} ago`;
}

// Chrome: navigation, node card, recording pill

function renderNav() {
  const s = model.status;
  const problems = problemCount();
  const counts = {
    apps: s ? apps().length : undefined,
    sidecars: s?.sidecars?.length,
    mesh: s?.mesh?.peers?.length,
  };
  $("nav").replaceChildren(
    ...Object.entries(VIEWS).flatMap(([key, v]) => [
      ...(v.group ? [h("div", { class: "nav-group" }, v.group)] : []),
      h(
        "button",
        {
          class: key === view.current ? "active" : "",
          "aria-current": key === view.current ? "page" : null,
          title: `${v.title}  ⌘${Object.keys(VIEWS).indexOf(key) + 1}`,
          onclick: () => go(key),
        },
        icon(v.icon),
        h("span", { class: "nav-label" }, v.title),
        key === "home" && problems
          ? h("span", { class: "count bad", title: `${problems} need attention` }, problems)
          : counts[key] !== undefined
            ? h("span", { class: "count" }, counts[key])
            : null,
      ),
    ]),
  );
}

function renderNode() {
  const card = $("node");
  if (model.connected && model.status) {
    const s = model.status;
    const uptime = s.uptime_s + (Date.now() - model.statusAt) / 1000;
    card.title = `pid ${s.pid}`;
    card.replaceChildren(
      h("span", { class: "dot ok" }),
      h("span", { class: "node-name" }, s.node),
      h("span", { class: "meta" }, `up ${duration(uptime)}`),
    );
    return;
  }
  // The Start button lives in the view, not here, so it shows once.
  card.title = model.reason ?? "";
  card.replaceChildren(
    h("span", { class: model.connected ? "dot warn" : "dot err" }),
    h("span", { class: "node-name" }, model.connected ? "Connecting…" : "Node not running"),
  );
}

function renderRecording() {
  const pill = $("recording");
  const active = model.status?.active_sensors ?? [];
  pill.hidden = active.length === 0;
  if (active.length) {
    const who = active.map((a) => `${a.plugin} (${a.sensor})`).join(", ");
    pill.replaceChildren(h("span", { class: "dot rec" }), `Recording: ${who}`);
  }
}

// Shown while no node answers, or while the first status is on its way.
function notRunning() {
  if (model.connected) {
    return emptyState({ icon: "reload", title: "Waiting for the node", text: "Its first report is on the way." });
  }
  return h(
    "div",
    { class: "down" },
    emptyState({
      icon: "plug",
      title: "Node not running",
      text: model.canStart
        ? "bricolo-ui keeps trying to reach it. Start it here, or from a terminal:"
        : "bricolo-ui keeps trying to reach it. Start it from a terminal:",
      action: h(
        "div",
        { class: "down-actions" },
        model.canStart
          ? h("button", { class: "btn primary", onclick: (e) => startNode(e.currentTarget) }, icon("play", { size: 12 }), "Start node")
          : null,
        h("code", { class: "cmd" }, "bricolo run"),
      ),
    }),
    model.reason ? h("details", { class: "reason" }, h("summary", {}, "Why"), h("p", { class: "mono" }, model.reason)) : null,
  );
}

// Card heights: each card in a `.grid` spans as many 2px rows as its
// height needs, so short cards stay short and tall ones grow. A
// ResizeObserver runs after layout and before paint, so a card never
// shows at the wrong size, and it follows wrapping when the window
// resizes.
//
// A widget also picks its width: its value column is as wide as its widest
// value (values never wrap), and the card spans as many grid columns as it
// takes to leave its labels at least WIDGET_LABEL px.
const ROW = 2;
const GAP = 14;
const WIDGET_LABEL = 130;

function widgetSpan(card) {
  const grid = card.parentElement;
  const tracks = getComputedStyle(grid).gridTemplateColumns.split(" ").map(parseFloat).filter((n) => n > 0);
  const dd = card.querySelector(".widget-rows dd");
  if (!tracks.length || !dd) return 1;
  const col = tracks[0];
  const box = card.getBoundingClientRect().width - card.querySelector(".widget-rows").getBoundingClientRect().width;
  const need = box + WIDGET_LABEL + GAP + dd.getBoundingClientRect().width;
  let span = 1;
  while (span < tracks.length && span * col + (span - 1) * GAP < need) span += 1;
  return span;
}

function sizeWidget(target) {
  // Measure against the explicit columns only: a span left from a wider
  // window would add an implicit track.
  target.style.gridColumnEnd = "";
  target.style.gridColumnEnd = `span ${widgetSpan(target)}`;
}

function fillLastRow(grid) {
  const tracks = getComputedStyle(grid).gridTemplateColumns.split(" ").map(parseFloat).filter((n) => n > 0);
  const widgets = [...grid.children].filter((c) => c.classList.contains("widget"));
  if (tracks.length < 2 || !widgets.length) return;
  const g = grid.getBoundingClientRect();
  const rects = new Map(widgets.map((w) => [w, w.getBoundingClientRect()]));
  const last = widgets.reduce((a, b) => {
    const x = rects.get(a);
    const y = rects.get(b);
    return y.top > x.top + 1 || (Math.abs(y.top - x.top) <= 1 && y.left > x.left) ? b : a;
  });
  const me = rects.get(last);
  const blocked = widgets.some((w) => {
    const r = rects.get(w);
    return w !== last && r.left >= me.right && r.bottom > me.top + 1;
  });
  if (blocked) return;
  const col = tracks[0];
  const start = Math.round((me.left - g.left) / (col + GAP));
  const rest = tracks.length - start;
  const span = Math.round((me.width + GAP) / (col + GAP));
  if (rest > span) last.style.gridColumnEnd = `span ${rest}`;
}

const sizer = new ResizeObserver((entries) => {
  const grids = new Set();
  for (const { target } of entries) {
    // The grid itself changed width: its columns did too, so every widget
    // is sized again from scratch, with no span left from before.
    if (target.classList.contains("grid")) {
      const widgets = [...target.children].filter((c) => c.classList.contains("widget"));
      for (const w of widgets) w.style.gridColumnEnd = "";
      for (const w of widgets) sizeWidget(w);
      grids.add(target);
      continue;
    }
    if (target.classList.contains("widget")) {
      sizeWidget(target);
      grids.add(target.parentElement);
    }
    const height = target.getBoundingClientRect().height;
    target.style.gridRowEnd = `span ${Math.max(1, Math.ceil((height + GAP) / ROW))}`;
  }
  // Once every widget has its place, the one that ends the grid on screen
  // (lowest, then rightmost) takes the rest of its row when nothing else
  // reaches into it, so a wide widget before it leaves no hole.
  for (const grid of grids) fillLastRow(grid);
});

function pack(root) {
  // Each render replaces the cards; stop watching the old ones.
  sizer.disconnect();
  for (const tile of root.querySelectorAll(".grid > .tile")) sizer.observe(tile);
  for (const grid of root.querySelectorAll(".grid")) sizer.observe(grid);
}

// A small button that copies `text`, then says so.
function copyButton(text, what) {
  return h(
    "button",
    {
      class: "copy",
      title: `Copy ${what}`,
      "aria-label": `Copy ${what}`,
      onclick: async () => {
        try {
          await navigator.clipboard.writeText(text);
          toast(`Copied ${what}.`);
        } catch (e) {
          toast(`Could not copy: ${e}`, "error");
        }
      },
    },
    icon("copy", { size: 13 }),
  );
}

// Sidecars and mesh

function table(headers, rows) {
  return h(
    "div",
    { class: "table-wrap" },
    h("table", {}, h("thead", {}, h("tr", {}, headers.map((t) => (typeof t === "string" ? h("th", {}, t) : h("th", { class: t.cls }, t.text))))), h("tbody", {}, rows)),
  );
}

function renderSidecars() {
  if (!model.connected || !model.status) return notRunning();
  const sidecars = model.status?.sidecars ?? [];
  if (!sidecars.length) {
    return emptyState({
      icon: "server",
      title: "No sidecars",
      text: "Local HTTP servers listed in node.toml under [sidecars.<name>] show here. Add one with bricolo sidecar add.",
    });
  }
  // A ready sidecar's error is from before its last restart.
  const lastError = (s) => (s.error && s.state === "ready" ? `Before the last restart: ${s.error}` : s.error);
  return table(
    // A narrow window drops PID and Restarts, and puts the last error under
    // the name, so Restart stays on screen.
    ["Name", "State", "Port", { text: "PID", cls: "opt" }, { text: "Restarts", cls: "opt" }, { text: "Last error", cls: "wide-only" }, ""],
    sidecars.map((s) =>
      h(
        "tr",
        {},
        h(
          "td",
          {},
          h("strong", {}, s.name),
          s.error ? h("div", { class: `narrow-only peer-error${s.state === "ready" ? " old" : ""}` }, lastError(s)) : null,
        ),
        h("td", {}, badge(s.state, s.state)),
        h("td", { class: "mono" }, s.port),
        h("td", { class: "mono opt" }, s.pid ?? "–"),
        h("td", { class: "opt" }, s.restarts),
        // A ready sidecar's old error is history, not trouble.
        h("td", { class: s.state === "ready" ? "last-error wide-only meta small" : "last-error wide-only err" }, lastError(s) ?? ""),
        h(
          "td",
          {},
          h("button", { class: "btn sm", onclick: () => send({ cmd: "restart-sidecar", sidecar: s.name }) }, icon("reload", { size: 13 }), "Restart"),
        ),
      ),
    ),
  );
}

// rules.toml: where routable capabilities run.
function routes() {
  const rules = model.status?.rules;
  const entries = Object.entries(rules?.routes ?? {});
  return h(
    "div",
    { class: "routes" },
    h("div", { class: "section-title" }, "Routes"),
    entries.length
      ? table(
          ["Capability", "Targets, in order"],
          entries.map(([cap, targets]) => h("tr", {}, h("td", {}, h("strong", {}, cap)), h("td", {}, targets.join(" → ")))),
        )
      : h("p", { class: "meta" }, "rules.toml routes nothing, so every capability runs on this node."),
    entries.length && rules ? h("p", { class: "meta" }, `Each target gets ${rules.timeout_secs} s before the next one is tried.`) : null,
  );
}

// The MCP gateway: whether it runs, its HTTP address, and buttons that
// add it to Claude Code or Claude Desktop.
function agents() {
  const mcp = model.status?.mcp ?? {};
  const connect = async (client, label) => {
    const answer = await send({ cmd: "mcp-connect", client });
    if (answer && !answer.error) toast(answer.message ?? `Connected ${label}.`);
  };
  return h(
    "div",
    { class: "routes" },
    h("div", { class: "section-title" }, "AI agents"),
    mcp.enabled
      ? h(
          "div",
          {},
          h(
            "p",
            { class: "meta" },
            "The MCP gateway lets an agent use every node in the mesh. ",
            mcp.http ? `It also listens on http://${mcp.http}/mcp; bricolo mcp url prints the token and the Funnel address.` : "HTTP is off; set [mcp] listen in node.toml to reach it from a cloud agent.",
          ),
          h(
            "div",
            { class: "buttons" },
            h("button", { class: "btn primary", onclick: () => connect("claude-code", "Claude Code") }, "Connect to Claude Code"),
            h("button", { class: "btn", onclick: () => connect("claude-desktop", "Claude Desktop") }, "Connect to Claude Desktop"),
          ),
        )
      : h("p", { class: "meta" }, "The MCP gateway is off: node.toml has [mcp] enabled = false."),
  );
}

function renderMesh() {
  if (!model.connected || !model.status) return notRunning();
  const mesh = model.status?.mesh ?? {};
  if (!mesh.enabled) {
    return h(
      "div",
      {},
      emptyState({
        icon: "network",
        title: "The mesh is off",
        text: "node.toml has [mesh] enabled = false, so this node reaches no peers.",
      }),
      routes(),
      agents(),
    );
  }
  const info = h(
    "dl",
    { class: "kv" },
    h("dt", {}, "Endpoint id"),
    h("dd", { class: "mono" }, h("span", { class: "id", title: mesh.id }, mesh.id), copyButton(mesh.id, "endpoint id")),
    ...(mesh.transports ?? []).flatMap((t) => [
      h("dt", {}, t.kind === "tailscale" ? "Tailscale" : "iroh"),
      h("dd", {}, t.detail, h("div", { class: "mono meta" }, (t.bound ?? []).join(", ") || "–")),
    ]),
  );
  const peers = mesh.peers ?? [];
  if (!peers.length) {
    return h(
      "div",
      {},
      info,
      emptyState({ icon: "network", title: "No peers", text: "Pair a device with bricolo id and bricolo peer add.", small: true }),
      routes(),
      agents(),
    );
  }
  return h(
    "div",
    {},
    info,
    table(
      ["Peer", "State", { text: "Incoming", cls: "opt" }, "Last seen", "May call"],
      peers.map((p) =>
        h(
          "tr",
          {},
          h("td", {}, h("strong", {}, p.name), h("div", { class: "row-id" }, h("span", { class: "mono meta", title: p.id }, `${p.id.slice(0, 16)}…`), copyButton(p.id, `${p.name}'s id`)),
            // Why a peer is down, under its name: a last column runs off a
            // narrow window. A connected peer's old error is history.
            p.state !== "connected" && p.last_error ? h("div", { class: "peer-error" }, p.last_error) : null,
          ),
          h("td", {}, badge(p.state, p.state), p.via ? h("div", { class: "meta" }, `via ${p.via === "tailscale" ? "Tailscale" : "iroh"}`) : null),
          h("td", { class: "opt" }, p.incoming),
          h("td", {}, ago(p.last_seen)),
          h("td", { class: "wrap" }, (p.allow ?? []).join(", ") || "nothing"),
        ),
      ),
    ),
    routes(),
    agents(),
  );
}

// Logs

function matches(line) {
  if (filters.source && line.plugin !== filters.source) return false;
  if (LEVELS.indexOf(line.level) < LEVELS.indexOf(filters.level)) return false;
  if (filters.text) {
    const needle = filters.text.toLowerCase();
    if (!line.message.toLowerCase().includes(needle) && !line.plugin.toLowerCase().includes(needle)) {
      return false;
    }
  }
  return true;
}

function logRow(line) {
  return h(
    "div",
    { class: `log-line ${line.level}` },
    h("span", { class: "time" }, clock(line.ts_ms)),
    h("span", { class: `lvl ${line.level}` }, line.level),
    h("span", { class: "src", title: line.plugin }, line.plugin),
    // Sidecars often print their own level first ("INFO:     …"); the
    // level column already says it.
    h("span", { class: "msg" }, line.message.replace(/^(TRACE|DEBUG|INFO|WARN(ING)?|ERROR):\s+/, "")),
  );
}

function appendLog(line) {
  const list = document.querySelector(".log-list");
  if (!list || !matches(line)) return;
  list.append(logRow(line));
  while (list.childElementCount > MAX_LOG_ROWS) list.firstElementChild.remove();
  if (filters.follow) list.scrollTop = list.scrollHeight;
}

function sources() {
  const names = new Set(["node"]);
  for (const p of model.status?.plugins ?? []) names.add(p.name);
  for (const s of model.status?.scripts ?? []) names.add(`${s.name}.lua`);
  for (const s of model.status?.sidecars ?? []) names.add(`sidecar:${s.name}`);
  for (const l of model.logs) names.add(l.plugin);
  return [...names].sort();
}

function renderLogs() {
  const list = h("div", { class: "log-list" });
  const fill = () => {
    list.replaceChildren(...model.logs.filter(matches).slice(-MAX_LOG_ROWS).map(logRow));
    if (filters.follow) requestAnimationFrame(() => (list.scrollTop = list.scrollHeight));
  };
  const source = h(
    "select",
    {
      onchange: (e) => {
        filters.source = e.target.value;
        fill();
      },
    },
    h("option", { value: "" }, "All sources"),
    sources().map((name) => h("option", { value: name, selected: name === filters.source }, name)),
  );
  const level = h(
    "select",
    {
      onchange: (e) => {
        filters.level = e.target.value;
        fill();
      },
    },
    LEVELS.map((l) => h("option", { value: l, selected: l === filters.level }, `${l} and up`)),
  );
  const search = h("input", {
    type: "search",
    placeholder: "Filter text",
    value: filters.text,
    oninput: (e) => {
      filters.text = e.target.value;
      fill();
    },
  });
  const follow = h(
    "label",
    {},
    h("input", {
      type: "checkbox",
      checked: filters.follow,
      onchange: (e) => {
        filters.follow = e.target.checked;
        if (filters.follow) list.scrollTop = list.scrollHeight;
      },
    }),
    "Follow",
  );
  const clear = h(
    "button",
    {
      class: "btn",
      onclick: () => {
        model.logs = [];
        fill();
      },
    },
    "Clear",
  );
  clear.className = "btn ghost";
  fill();
  return h("div", { class: "logs" }, h("div", { class: "toolbar", "data-header": "" }, source, level, search, follow, clear), list);
}

// Rendering

// A re-render replaces the elements, so the one with the keyboard, marked
// with `data-focus`, gets it back, with its caret.
function keepingFocus(draw) {
  const active = document.activeElement;
  const key = active?.dataset?.focus;
  let start = null;
  let end = null;
  try {
    start = active.selectionStart;
    end = active.selectionEnd;
  } catch {
    // Number and time inputs have no selection.
  }
  draw();
  // Only when the redraw dropped the keyboard: a drawer that just opened
  // has already taken it.
  if (!key || (document.activeElement && document.activeElement !== document.body)) return;
  const again = document.querySelector(`[data-focus="${CSS.escape(key)}"]`);
  if (!again) return;
  again.focus();
  try {
    if (start !== null) again.setSelectionRange(start, end);
  } catch {
    // As above.
  }
}

function render() {
  keepingFocus(draw);
}

function draw() {
  renderNav();
  renderNode();
  renderRecording();
  $("title").textContent = VIEWS[view.current].title;
  renderDrawer();
  const root = $("view");
  // The logs view keeps its scroll position and filters; only switching
  // to it, or a change of node, redraws it.
  if (view.current === "logs" && root.dataset.view === "logs" && root.dataset.connected === String(model.connected)) {
    return;
  }
  // The library keeps its filter box; a status tick redraws only the
  // grants and the list. A change of node asks it again.
  if (view.current === "library" && root.dataset.view === "library") {
    if (root.dataset.connected === String(model.connected)) {
      updateLibrary();
    } else {
      root.dataset.connected = String(model.connected);
      refreshLibrary();
    }
    return;
  }
  root.dataset.view = view.current;
  root.dataset.connected = String(model.connected);
  root.classList.toggle("fill", view.current === "logs");
  const needsNode = view.current !== "logs" && view.current !== "library";
  const content = needsNode && (!model.connected || !model.status) ? notRunning() : VIEWS[view.current].render();
  // A view's top toolbar, marked data-header, sits in the header beside the
  // title, which saves a row in the short dropdown window.
  const tools = content.querySelector?.(":scope > .toolbar[data-header]");
  $("toolbar").replaceChildren(...(tools ? [tools] : []));
  root.replaceChildren(content);
  pack(root);
}

// From the Library's "used by": open that plugin's or script's drawer.
function openCard(kind, name) {
  view.current = "apps";
  openApp(`${kind}:${name}`, "health");
}

async function main() {
  Object.assign(actions, { send, quietly, render, toast, go, showLogs });
  const snap = await invoke("snapshot");
  model.connected = snap.connected;
  model.reason = snap.reason;
  model.status = snap.status;
  model.statusAt = Date.now();
  model.ui = snap.ui ?? { tiles: {}, widgets: {}, panels: [] };
  model.logs = snap.logs ?? [];
  model.canStart = snap.can_start;
  if (snap.view) view.current = snap.view in VIEWS ? snap.view : (OLD_VIEWS[snap.view] ?? "home");
  // Before the first render: `--view library` draws it at once.
  setupLibrary({
    request: quietly,
    send,
    status: () => model.status,
    connected: () => model.connected,
    openCard,
  });
  render();

  listen("node", (e) => {
    model.connected = e.connected;
    model.reason = e.reason ?? "";
    if (!e.connected) {
      model.status = null;
      model.ui = { tiles: {}, widgets: {}, panels: [] };
    }
    render();
  });
  listen("status", (status) => {
    model.status = status;
    model.statusAt = Date.now();
    model.connected = true;
    render();
  });
  listen("ui", (ui) => {
    model.ui = ui ?? { tiles: {}, widgets: {}, panels: [] };
    if (view.current !== "logs" || view.open) render();
  });
  listen("logs", (lines) => {
    model.logs = lines;
    if (view.current === "logs") {
      document.getElementById("view").dataset.view = "";
      render();
    }
  });
  listen("log", (line) => {
    model.logs.push(line);
    if (model.logs.length > MAX_LOG_ROWS) model.logs.shift();
    if (view.current === "logs") appendLog(line);
  });
  setupHotkeys({ redraw: render, send: quietly });
  $("search").prepend(icon("search", { size: 14 }));
  $("search").onclick = openPalette;
  document.addEventListener("keydown", (e) => {
    if (isRecording() || isPaletteOpen()) return;
    const typing = /^(INPUT|SELECT|TEXTAREA)$/.test(document.activeElement?.tagName ?? "");
    const n = Number(e.key);
    if ((e.metaKey || e.ctrlKey) && n >= 1 && n <= Object.keys(VIEWS).length) {
      e.preventDefault();
      go(Object.keys(VIEWS)[n - 1]);
    } else if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      openPalette();
    } else if (e.key === "/" && !typing) {
      e.preventDefault();
      openPalette();
    } else if (e.key === "Escape") {
      // Esc closes the drawer first, then puts the hub away, as its chord
      // does (cmd+` by default).
      if (view.open) closeApp();
      else invoke("hide_hub");
    }
  });
  // The node card's uptime. While the node is down the card holds the
  // Start button, which must not be redrawn under the pointer.
  setInterval(() => {
    if (model.connected && model.status) renderNode();
  }, 1000);
}

main();
