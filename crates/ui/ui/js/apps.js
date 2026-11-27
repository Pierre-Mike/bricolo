// The Apps view and an app's drawer. Plugins and scripts are one list; a
// card shows the name, state, one line and the main switch, and a click
// opens the drawer with the tabs Now, Controls, Settings, Shortcuts and
// Health.

import { chordLabel, hotkeyChip, hotkeyErrors, shortcutName } from "./chord.js";
import { badge, commands, emptyState, h, toggle } from "./dom.js";
import { icon } from "./icons.js";
import { actions, apps, findApp, health, mainSwitch, model, pinKey, prefs, savePrefs, view } from "./store.js";
import { widgetBody } from "./widget.js";

const act = (action) => actions.send({ cmd: "ui-action", action });

// A switch that does not open the card it sits on.
function quietToggle(on, onchange, label) {
  return h("span", { class: "stop", onclick: (e) => e.stopPropagation() }, toggle(on, onchange, label));
}

function switchFor(a, sw) {
  return quietToggle(sw.on, (on) => act({ kind: "switch", owner: a.owner, id: sw.id, on }), sw.label || sw.id);
}

export function dot(a) {
  return h("span", { class: `dot ${health(a)}` });
}

// One line under the name: what the app says, else its error, else its
// widget's first gauge or row.
function glance(a) {
  // The card keeps the error short ("init failed"); the drawer has all of it.
  if (a.item.error && health(a) !== "ok") {
    const tone = health(a) === "warn" ? "warn-text" : "error-text";
    return h("p", { class: `glance ${tone}`, title: a.item.error }, a.item.error.split(":")[0]);
  }
  if (a.tile?.status_text) return h("p", { class: "glance" }, a.tile.status_text);
  const row = a.widget?.gauges?.[0] ?? a.widget?.rows?.[0];
  if (row) return h("p", { class: "glance" }, h("span", { class: "meta" }, `${row.label} `), h("b", {}, row.value));
  if (a.item.state !== "running") return h("p", { class: "glance meta" }, a.item.state);
  return null;
}

function appCard(a) {
  const main = mainSwitch(a.tile);
  const hotkeys = Object.values(a.item.hotkeys ?? {});
  const controls = (a.tile?.switches.length ?? 0) + (a.tile?.buttons.length ?? 0);
  return h(
    "article",
    {
      class: `tile app-card ${health(a) === "off" ? "dim" : ""}`,
      "data-card": a.key,
      // Re-renders give the keyboard back to this card (hub.js keepingFocus).
      "data-focus": `card:${a.key}`,
      tabindex: "0",
      onclick: () => openApp(a.key),
      onkeydown: (e) => e.key === "Enter" && openApp(a.key),
    },
    h(
      "div",
      { class: "tile-head" },
      h(
        "h3",
        { title: a.item.version ? `${a.label} v${a.item.version}` : a.label },
        dot(a),
        h("span", { class: "name" }, a.label),
        a.kind === "script" ? h("span", { class: "kind-icon", title: "Lua script" }, icon("script", { size: 13 })) : null,
      ),
      main ? h("span", { class: "switch-label" }, main.label || main.id, switchFor(a, main)) : null,
    ),
    glance(a) ?? h("p", { class: "glance meta" }, "Running"),
    hotkeys.length || controls > (main ? 1 : 0)
      ? h(
          "div",
          { class: "card-foot" },
          hotkeys.slice(0, 3).map((chord) => h("kbd", {}, chordLabel(chord))),
          hotkeys.length > 3 ? h("span", { class: "meta" }, `+${hotkeys.length - 3}`) : null,
          controls > (main ? 1 : 0)
            ? h("span", { class: "meta controls-count", title: `${controls} controls` }, icon("toggle", { size: 12 }), String(controls))
            : null,
        )
      : null,
  );
}

export function renderApps() {
  const q = view.filter.toLowerCase();
  const list = apps()
    .filter((a) => view.kind === "all" || a.kind === view.kind)
    .filter((a) => !q || `${a.label} ${a.tile?.status_text ?? ""} ${a.tile?.title ?? ""}`.toLowerCase().includes(q))
    .sort((x, y) => x.label.localeCompare(y.label));
  const seg = (kind, label) =>
    h(
      "button",
      {
        class: view.kind === kind ? "on" : "",
        onclick: () => {
          view.kind = kind;
          actions.render();
        },
      },
      label,
    );
  return h(
    "div",
    {},
    // Nothing to filter until the node runs an app.
    apps().length === 0 ? null : h(
      "div",
      { class: "toolbar apps-toolbar", "data-header": "" },
      h("input", {
        type: "search",
        placeholder: "Filter apps",
        value: view.filter,
        "data-focus": "apps-filter",
        oninput: (e) => {
          view.filter = e.target.value;
          actions.render();
        },
      }),
      h("div", { class: "seg" }, seg("all", "All"), seg("plugin", "Plugins"), seg("script", "Scripts")),
    ),
    list.length
      ? h("div", { class: "app-grid", onkeydown: moveInGrid }, list.map(appCard))
      : apps().length
        ? emptyState({
            icon: "search",
            title: view.kind === "all" ? "No app matches" : `No ${view.kind}s match`,
            text: view.filter
              ? `No ${view.kind === "all" ? "app" : view.kind} is called “${view.filter}”.`
              : `This node runs no ${view.kind}s.`,
          })
        : emptyState({
            icon: "apps",
            title: "No apps yet",
            text: "Start one from a terminal:",
            action: commands([
              ["bricolo new-script <name>", "a Lua script"],
              ["bricolo new <name>", "a plugin in Rust"],
              ["bricolo install <folder>", "install one"],
            ]),
          }),
  );
}

// The drawer

// On, but not running: it failed to start or stopped on an error.
function failedApp(a) {
  return a.item.state !== "disabled" && health(a) === "err";
}

const FAILED_TEXT = "Not running: it failed to start. Health says why.";

export function openApp(key, tab) {
  const a = findApp(key);
  if (!a) return;
  view.open = key;
  // A failed app opens on Health, which says why and offers Reload.
  view.tab = tab ?? (failedApp(a) ? "health" : a.tile?.status_text || a.widget ? "now" : a.tile ? "controls" : a.kind === "plugin" ? "settings" : "shortcuts");
  // Settings load with the drawer, so the tab counts are there from the
  // start and the tabs never shift.
  if (a.kind === "plugin") loadSettings(a);
  actions.render();
}

export function closeApp() {
  const key = view.open;
  view.open = null;
  actions.render();
  // Give the keyboard back to the card the drawer came from.
  if (key) document.querySelector(`[data-card="${CSS.escape(key)}"]`)?.focus();
}

// Arrow keys move between cards, row by row, the way Finder's icon view does.
function moveInGrid(e) {
  const step = { ArrowLeft: -1, ArrowRight: 1 }[e.key];
  const grid = e.currentTarget;
  const cards = [...grid.querySelectorAll(".app-card")];
  const at = cards.indexOf(document.activeElement);
  if (at < 0) return;
  const columns = getComputedStyle(grid).gridTemplateColumns.split(" ").length;
  const delta = step ?? { ArrowUp: -columns, ArrowDown: columns }[e.key];
  if (!delta) return;
  e.preventDefault();
  cards[Math.min(cards.length - 1, Math.max(0, at + delta))].focus();
}

// Tab keys: ← and → move along the drawer's tabs.
function moveTab(e, a) {
  const step = { ArrowLeft: -1, ArrowRight: 1 }[e.key];
  if (!step) return;
  e.preventDefault();
  const keys = TABS.map(([key]) => key);
  const next = keys[(keys.indexOf(view.tab) + step + keys.length) % keys.length];
  view.tab = next;
  if (next === "settings") loadSettings(a);
  actions.render();
  document.querySelector(".drawer .tabs button.on")?.focus();
}

const TABS = [
  ["now", "Now"],
  ["controls", "Controls"],
  ["settings", "Settings"],
  ["shortcuts", "Shortcuts"],
  ["health", "Health"],
];

export function renderDrawer() {
  const root = document.getElementById("drawer-root");
  const a = view.open ? findApp(view.open) : null;
  if (!a) {
    view.open = null;
    root.replaceChildren();
    delete root.dataset.open;
    return;
  }
  const main = mainSwitch(a.tile);
  const settings = view.settings[a.item.name];
  const counts = {
    controls: (a.tile?.switches.length ?? 0) + (a.tile?.buttons.length ?? 0) || null,
    settings: settings?.settings?.filter((s) => !s.hidden).length || null,
    shortcuts: Object.keys(a.item.hotkeys ?? {}).length || null,
    health: health(a) === "err" ? "!" : health(a) === "warn" ? "!w" : null,
  };
  // Only a drawer that just opened slides in; re-renders keep it still.
  const opening = root.dataset.open !== a.key;
  root.dataset.open = a.key;
  const body = { now: tabNow, controls: tabControls, settings: tabSettings, shortcuts: tabShortcuts, health: tabHealth }[view.tab](a);
  root.replaceChildren(
    h("div", { class: opening ? "scrim opening" : "scrim", onclick: closeApp }),
    h(
      "aside",
      { class: opening ? "drawer opening" : "drawer", role: "dialog", "aria-label": a.label },
      h(
        "div",
        { class: "drawer-head" },
        h(
          "div",
          { class: "tile-head" },
          h(
            "h2",
            {},
            dot(a),
            h("span", { class: "name" }, a.label),
            a.item.version ? h("span", { class: "version" }, `v${a.item.version}`) : null,
            h("span", { class: "badge kind" }, icon(a.kind, { size: 11 }), a.kind),
          ),
          h(
            "div",
            { class: "row" },
            main ? h("span", { class: "meta" }, main.label || main.id) : null,
            main ? switchFor(a, main) : null,
            h("button", { class: "btn ghost icon-only", title: "Close (Esc)", "aria-label": "Close", onclick: closeApp }, icon("x")),
          ),
        ),
        h(
          "nav",
          { class: "tabs", role: "tablist", onkeydown: (e) => moveTab(e, a) },
          TABS.map(([key, label]) =>
            h(
              "button",
              {
                class: view.tab === key ? "on" : "",
                role: "tab",
                "aria-selected": String(view.tab === key),
                "data-focus": view.tab === key ? "drawer-tab" : null,
                tabindex: view.tab === key ? "0" : "-1",
                onclick: () => {
                  view.tab = key;
                  if (key === "settings") loadSettings(a);
                  actions.render();
                  document.querySelector(".drawer .tabs button.on")?.focus({ preventScroll: true });
                },
              },
              label,
              counts[key] != null
                ? h("span", { class: `n ${counts[key] === "!" ? "bad" : counts[key] === "!w" ? "warn" : ""}` }, counts[key] === "!w" ? "!" : counts[key])
                : null,
            ),
          ),
        ),
      ),
      h("div", { class: "drawer-body", role: "tabpanel" }, body),
      settingsFooter(a),
    ),
  );
  for (const box of root.querySelectorAll("[data-tail]")) {
    box.scrollTop = box.scrollHeight;
    box.classList.toggle("clipped", box.scrollHeight > box.clientHeight);
  }
  // A drawer that just opened takes the keyboard, on its current tab.
  if (opening) document.querySelector(".drawer .tabs button.on")?.focus({ preventScroll: true });
}

function nothing(text, name = "inbox") {
  return emptyState({ icon: name, text, small: true });
}

function widgetCard(a) {
  const w = a.widget;
  return h(
    "div",
    { class: "tile widget" },
    h("div", { class: "tile-head" }, h("h3", {}, w.title || a.label)),
    widgetBody(w),
  );
}

function tabNow(a) {
  if (failedApp(a)) return [nothing(FAILED_TEXT)];
  const shown = !prefs.hiddenWidgets.has(a.owner);
  return [
    a.tile?.status_text
      ? h("div", { class: "tile" }, h("div", { class: "meta" }, a.tile.title || "Status"), h("div", { class: "status-big" }, a.tile.status_text))
      : null,
    a.widget ? widgetCard(a) : null,
    a.widget
      ? h(
          "div",
          { class: "row between" },
          h("span", {}, "Show this widget on Home"),
          toggle(shown, (on) => {
            if (on) prefs.hiddenWidgets.delete(a.owner);
            else prefs.hiddenWidgets.add(a.owner);
            savePrefs();
            actions.render();
          }, "Show on Home"),
        )
      : null,
    !a.tile?.status_text && !a.widget ? nothing("This app shows no status line and no widget.") : null,
  ];
}

function tabControls(a) {
  const tile = a.tile;
  if (failedApp(a)) return [nothing(FAILED_TEXT)];
  if (!tile || (!tile.switches.length && !tile.buttons.length)) {
    const hasShortcuts = Object.keys(a.item.hotkeys ?? {}).length > 0;
    const text = a.item.state === "disabled" ? "Turned off. Turn it on in Health." : hasShortcuts ? "This app has no controls. Its shortcuts run it." : "This app has no controls.";
    return [nothing(text)];
  }
  return [
    tile.switches.length
      ? h(
          "div",
          { class: "tile" },
          tile.switches.map((sw) => {
            const key = pinKey(a.owner, sw.id);
            const pinned = prefs.pins.has(key);
            return h(
              "div",
              { class: "ctl-row" },
              h("span", { class: "grow" }, sw.label || sw.id),
              h(
                "button",
                {
                  class: `pin ${pinned ? "on" : ""}`,
                  title: pinned ? "Unpin from Home" : "Pin to Home",
                  "aria-pressed": String(pinned),
                  onclick: () => {
                    if (pinned) prefs.pins.delete(key);
                    else prefs.pins.add(key);
                    savePrefs();
                    actions.render();
                  },
                },
                pinned ? "Pinned" : "Pin",
              ),
              switchFor(a, sw),
            );
          }),
        )
      : null,
    tile.buttons.length
      ? h(
          "div",
          { class: "tile" },
          h("div", { class: "meta" }, "Actions"),
          h("div", { class: "buttons" }, tile.buttons.map((b) => h("button", { class: "btn", onclick: () => act({ kind: "button", owner: a.owner, id: b.id }) }, b.label || b.id))),
        )
      : null,
  ];
}

function tabShortcuts(a) {
  const hotkeys = Object.entries(a.item.hotkeys ?? {});
  if (!hotkeys.length) return [nothing("This app binds no shortcuts.")];
  const asked = a.item.hotkey_defaults ?? {};
  return [
    h("p", { class: "meta" }, "Click a shortcut, then press the new keys. Once you change one, a reset arrow puts back the app's own."),
    h(
      "div",
      { class: "tile" },
      hotkeys.map(([id, chord]) =>
        h(
          "div",
          { class: "ctl-row" },
          h("span", { class: "shortcut-id", title: `Hotkey id: ${id}` }, shortcutName(id)),
          h("span", { class: "grow" }, hotkeyChip({ kind: a.kind, name: a.item.name, id, chord, asked: asked[id], send: actions.quietly })),
          a.kind === "plugin"
            ? h("button", { class: "btn sm", title: "Deliver this hotkey now, as if pressed", onclick: () => actions.send({ cmd: "fire", plugin: a.item.name, hotkey: id }) }, "Run")
            : null,
        ),
      ),
    ),
    hotkeyErrors(a.kind, a.item.name).map((m) => h("p", { class: "error" }, m)),
  ];
}

function tabHealth(a) {
  const item = a.item;
  const enabled = item.state !== "disabled";
  // On, but not running: it failed to start, so Reload is the way out.
  const failed = enabled && health(a) === "err";
  const target = { [a.kind]: item.name };
  const reload = a.kind === "plugin" ? { cmd: "reload", plugin: item.name } : { cmd: "reload-script", script: item.name };
  const lines = model.logs.filter((l) => l.plugin === a.owner).slice(-15);
  return [
    // A running app with an error is a warning, amber like its dot.
    item.error ? h("p", { class: health(a) === "warn" ? "error warn" : "error" }, item.error) : null,
    h(
      "dl",
      { class: "kv tile" },
      h("dt", {}, "State"),
      h("dd", {}, badge(failed ? "failed" : item.state, item.state)),
      h("dt", {}, "Restarts"),
      h("dd", {}, item.restarts ?? 0),
      h("dt", {}, "Timers"),
      h("dd", {}, item.timers ?? 0),
      h("dt", {}, "Kind"),
      h("dd", {}, a.kind === "plugin" ? "WebAssembly plugin: calls only the capabilities you approved" : "Lua script: calls every capability"),
    ),
    h(
      "div",
      // Opens on the newest line.
      { class: "log-mini", "data-tail": "" },
      lines.length ? lines.map((l) => h("div", { class: `lvl-${l.level}` }, `${new Date(l.ts_ms).toTimeString().slice(0, 8)}  ${l.message}`)) : h("div", { class: "meta" }, "No log lines yet."),
    ),
    h(
      "div",
      { class: "buttons" },
      h("button", { class: "btn", onclick: () => actions.showLogs(a.owner) }, icon("terminal", { size: 14 }), "All its logs"),
      enabled ? h("button", { class: failed ? "btn primary" : "btn", onclick: () => actions.send(reload) }, icon("reload", { size: 14 }), "Reload") : null,
      h(
        "button",
        { class: enabled ? (failed ? "btn" : "btn danger") : "btn primary", onclick: () => actions.send({ cmd: enabled ? "disable" : "enable", ...target }) },
        icon("power", { size: 14 }),
        enabled ? "Turn off" : "Turn on",
      ),
    ),
  ];
}

// Settings: drawn from the plugin's [settings] and [settings_ui]; saving
// writes [plugins.<name>] in node.toml and reloads the plugin.

async function loadSettings(a) {
  if (a.kind !== "plugin") return;
  const answer = await actions.quietly({ cmd: "settings", plugin: a.item.name });
  if (answer?.error) view.settingsError[a.item.name] = answer.error;
  else {
    view.settings[a.item.name] = answer;
    delete view.settingsError[a.item.name];
  }
  actions.render();
}

function human(key) {
  const unit = key.match(/^(.*)_(ms|s)$/);
  const base = (unit ? unit[1] : key).replace(/_/g, " ");
  return base.charAt(0).toUpperCase() + base.slice(1) + (unit ? ` (${unit[2]})` : "");
}

const same = (x, y) => JSON.stringify(x) === JSON.stringify(y);

function current(name, s) {
  const draft = view.drafts[name];
  return draft && s.key in draft ? draft[s.key] : s.value;
}

function setDraft(name, s, value) {
  const draft = (view.drafts[name] ??= {});
  if (same(value, s.value)) delete draft[s.key];
  else draft[s.key] = value;
  if (!Object.keys(draft).length) delete view.drafts[name];
  actions.render();
}

function input(name, s) {
  const value = current(name, s);
  const focus = `setting-${name}-${s.key}`;
  switch (s.kind) {
    case "bool":
      return toggle(value, (on) => setDraft(name, s, on), s.label ?? s.key);
    case "integer":
    case "float":
      return h("input", {
        type: "number",
        step: s.kind === "float" ? "any" : "1",
        value: String(value),
        "data-focus": focus,
        oninput: (e) => {
          const n = Number(e.target.value);
          if (e.target.value !== "" && Number.isFinite(n) && (s.kind === "float" || Number.isInteger(n))) setDraft(name, s, n);
        },
      });
    case "time":
      return h("input", { type: "time", value, "data-focus": focus, onchange: (e) => e.target.value && setDraft(name, s, e.target.value) });
    case "choice":
      return h(
        "select",
        { "data-focus": focus, onchange: (e) => setDraft(name, s, e.target.value) },
        // A path reads as its file name; the full path is the title.
        s.choices.map((c) => h("option", { value: c, selected: c === value, title: c }, c === "" ? "None (off)" : c.includes("/") ? c.split("/").pop() : c)),
      );
    case "multi":
      return h(
        "div",
        { class: "multi" },
        s.choices.map((c) =>
          h(
            "button",
            {
              class: value.includes(c) ? "on" : "",
              "aria-pressed": String(value.includes(c)),
              onclick: () => setDraft(name, s, value.includes(c) ? value.filter((x) => x !== c) : [...value, c]),
            },
            c,
          ),
        ),
      );
    case "list":
      return h("input", {
        type: "text",
        value: value.join(", "),
        "data-focus": focus,
        onchange: (e) =>
          setDraft(
            name,
            s,
            e.target.value.split(",").map((x) => x.trim()).filter(Boolean),
          ),
      });
    case "text":
      return h("input", { type: "text", class: "mono", value, "data-focus": focus, oninput: (e) => setDraft(name, s, e.target.value) });
    default:
      return h("code", { class: "mono", title: "Edit this one in node.toml" }, JSON.stringify(value));
  }
}

function field(name, s) {
  const value = current(name, s);
  const dirty = view.drafts[name] && s.key in view.drafts[name];
  const wide = s.kind === "multi";
  return h(
    "div",
    { class: `field ${dirty ? "changed" : ""} ${wide ? "wide" : ""}` },
    h(
      "div",
      { class: "field-name" },
      h("div", { class: "label" }, s.label ?? human(s.key), s.overridden && !dirty ? h("span", { class: "src", title: "Set in node.toml" }, "node.toml") : null),
      // The raw key, only when the label says something else.
      s.label ? h("div", { class: "key" }, s.key) : null,
    ),
    h("div", { class: "field-input" }, input(name, s)),
    h(
      "button",
      {
        class: "reset",
        title: `Back to the default: ${JSON.stringify(s.default)}`,
        hidden: same(value, s.default),
        "aria-label": "Back to the default",
        onclick: () => setDraft(name, s, s.default),
      },
      icon("reset", { size: 14 }),
    ),
    s.help ? h("div", { class: "help" }, s.help) : null,
  );
}

function tabSettings(a) {
  if (a.kind === "script") return [nothing("Scripts have no settings yet; a script keeps its options at the top of its file.")];
  const name = a.item.name;
  const error = view.settingsError[name];
  const answer = view.settings[name];
  if (!answer) return [error ? h("p", { class: "error" }, error) : nothing("Loading…")];
  const shown = answer.settings.filter((s) => !s.hidden);
  if (!shown.length) return [nothing("This plugin has no [settings].")];
  const plain = shown.filter((s) => !s.advanced);
  const advanced = shown.filter((s) => s.advanced);
  return [
    error ? h("p", { class: "error" }, error) : null,
    plain.length ? h("div", { class: "tile fields" }, plain.map((s) => field(name, s))) : null,
    advanced.length
      ? h(
          "details",
          {
            class: "advanced",
            open: view.advancedOpen.has(name) || plain.length === 0,
            ontoggle: (e) => (e.target.open ? view.advancedOpen.add(name) : view.advancedOpen.delete(name)),
          },
          h("summary", {}, `Advanced (${advanced.length}): paths and addresses`),
          h("div", { class: "tile fields" }, advanced.map((s) => field(name, s))),
        )
      : null,
    h(
      "p",
      { class: "note" },
      icon("info", { size: 14 }),
      h("span", {}, `Defaults come from plugin.toml. A change goes to [plugins.${name}] in node.toml and reloads ${name}.`),
    ),
  ];
}

function settingsFooter(a) {
  const name = a.item.name;
  const draft = view.drafts[name];
  if (!draft || view.tab !== "settings") return null;
  const n = Object.keys(draft).length;
  return h(
    "div",
    { class: "drawer-foot" },
    h("span", { class: "grow" }, h("b", {}, `${n} change${n === 1 ? "" : "s"}`), h("span", { class: "meta" }, ` · saving reloads ${name}`)),
    h(
      "button",
      {
        class: "btn",
        onclick: () => {
          delete view.drafts[name];
          actions.render();
        },
      },
      "Discard",
    ),
    h(
      "button",
      {
        class: "btn primary",
        onclick: async (e) => {
          e.target.disabled = true;
          const answer = await actions.quietly({ cmd: "set-settings", plugin: name, values: draft });
          if (answer?.error) {
            view.settingsError[name] = answer.error;
          } else {
            view.settings[name] = answer;
            delete view.drafts[name];
            delete view.settingsError[name];
            const failed = answer.status?.error;
            actions.toast(failed ? `Saved, but ${name}: ${failed}` : `Saved. ${name} reloaded.`, failed ? "error" : "info");
          }
          actions.render();
        },
      },
      "Save and reload",
    ),
  );
}
