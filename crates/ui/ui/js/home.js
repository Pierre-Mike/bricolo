// Home, the hub's first view: the setup walkthrough on a Mac until it is
// done, what needs attention, the switches you pinned, and every widget.

import { openApp } from "./apps.js";
import { badge, emptyState, h, toggle } from "./dom.js";
import { icon } from "./icons.js";
import { actions, apps, model, pinKey, prefs, savePrefs } from "./store.js";

// The library answer, for grants that apps need and the node lacks. Asked
// at most every 10 seconds while Home shows.
let library = null;
let libraryAt = 0;

async function refreshLibrary() {
  libraryAt = Date.now();
  const answer = await actions.quietly({ cmd: "library" });
  if (answer && !answer.error) {
    library = answer;
    actions.render();
  }
}

// A grant macOS holds for the node but shows it only from its next start:
// the node runs its program again, and Home asks again once it is back.
export async function restartNode() {
  await actions.quietly({ cmd: "restart-node" });
  setTimeout(refreshLibrary, 3000);
}

function restartButton(primary) {
  return h(
    "button",
    {
      class: `btn sm${primary ? " primary" : ""}`,
      title: "macOS shows this grant to the node only from its next start",
      onclick: restartNode,
    },
    "Restart node",
  );
}

// Each grant the node lacks that a running plugin's capabilities need.
// Scripts get every capability, so they would flag every grant; they are
// left out.
function missingGrants() {
  if (!library) return [];
  const running = new Set(apps().filter((a) => a.kind === "plugin" && a.item.state === "running").map((a) => a.item.name));
  const out = [];
  for (const g of library.grants ?? []) {
    if (g.state !== "missing" && g.state !== "not-asked" && g.state !== "restart") continue;
    const users = new Set();
    for (const c of library.capabilities ?? []) {
      const needs = Array.isArray(c.needs) ? c.needs : [c.needs];
      if (!needs.includes(g.name)) continue;
      for (const p of c.plugins ?? []) if (running.has(p)) users.add(p);
    }
    if (users.size) out.push({ grant: g.name, label: g.label ?? g.name, state: g.state, plugins: [...users] });
  }
  return out;
}

function problems() {
  const out = [];
  for (const a of apps()) {
    const state = a.item.state;
    if (state === "disabled" || state === "stopped") continue;
    if (state !== "running" || a.item.error) {
      out.push({
        level: state === "running" ? "warn" : "err",
        who: a.label,
        what: a.item.error ?? state,
        fix: h("button", { class: "btn sm", onclick: () => actions.send(a.kind === "plugin" ? { cmd: "reload", plugin: a.item.name } : { cmd: "reload-script", script: a.item.name }) }, "Reload"),
        open: () => openApp(a.key, "health"),
      });
    }
  }
  for (const s of model.status?.sidecars ?? []) {
    if (s.state === "ready" || s.state === "stopped" || (s.state === "starting" && !s.error)) continue;
    out.push({
      level: s.state === "unhealthy" ? "warn" : "err",
      who: `sidecar ${s.name}`,
      what: s.error ? `${s.state}: ${s.error}` : s.state,
      fix: h("button", { class: "btn sm", onclick: () => actions.send({ cmd: "restart-sidecar", sidecar: s.name }) }, "Restart"),
      open: () => actions.go("sidecars"),
    });
  }
  for (const g of missingGrants()) {
    const who = g.plugins.join(", ");
    if (g.state === "restart") {
      out.push({
        level: "warn",
        who: g.label,
        what: `granted; the node sees it after a restart, and ${who} may need it`,
        fix: restartButton(false),
        open: () => actions.go("library"),
      });
      continue;
    }
    out.push({
      level: "warn",
      who: g.label,
      what: `not granted; ${who} may need it`,
      fix: h(
        "button",
        {
          class: "btn sm",
          onclick: async () => {
            await actions.quietly({ cmd: "permissions", request: true, grant: g.grant });
            refreshLibrary();
          },
        },
        "Ask macOS",
      ),
      open: () => actions.go("library"),
    });
  }
  return out;
}

// What each macOS grant is for, in a few words, before the capability
// names the catalog lists for it.
const WHY = {
  accessibility: "Read and drive other apps' windows: what agents use to click, type and pick menu items.",
  post_events: "Press keys and move the pointer for you.",
  input_monitoring: "Hold the Fn key as a shortcut.",
  microphone: "Record your voice, for dictation.",
  camera: "Take photos.",
  screen_recording: "Take pictures of the screen and of app windows.",
};

// The capabilities that need `grant`, fully or for some calls.
function usersOf(grant) {
  return (library?.capabilities ?? [])
    .filter((c) => c.needs === grant || c.also === grant)
    .map((c) => (c.needs === grant ? c.name : `${c.name} (some calls)`));
}

// The walkthrough: each grant, what it unlocks and a button to ask for it.
// Shown on a Mac until every grant is answered or the user closes it.
function renderSetup() {
  if (!library || library.os !== "macos" || prefs.setupDone) return null;
  const grants = library.grants ?? [];
  const open = grants.filter((g) => g.state === "missing" || g.state === "not-asked" || g.state === "restart");
  if (!open.length) return null;
  const steps = grants.map((g, i) => {
    const done = g.state === "granted";
    const restart = g.state === "restart";
    const users = usersOf(g.name);
    return h(
      "li",
      { class: `setup-step${done ? " done" : ""}` },
      h("span", { class: "setup-num" }, done ? icon("check", { size: 13 }) : String(i + 1)),
      h(
        "div",
        { class: "grow" },
        h("div", {}, h("b", {}, g.label), done ? h("span", { class: "setup-state" }, "granted") : restart ? h("span", { class: "setup-state" }, "granted, restart the node to use it") : null),
        h("div", { class: "meta" }, WHY[g.name] ?? ""),
        users.length ? h("div", { class: "meta setup-users" }, `Used by ${users.join(", ")}`) : null,
      ),
      done
        ? null
        : restart
          ? h("div", { class: "setup-actions" }, restartButton(true))
          : h(
            "div",
            { class: "setup-actions" },
            h(
              "button",
              {
                class: "btn primary sm",
                onclick: async () => {
                  await actions.quietly({ cmd: "permissions", request: true, grant: g.name });
                  refreshLibrary();
                },
              },
              "Ask macOS",
            ),
            h(
              "button",
              {
                class: "btn sm",
                title: "Opens the grant's pane in System Settings, where you turn the node on",
                onclick: () => actions.quietly({ cmd: "open-settings", grant: g.name }),
              },
              "Open Settings",
            ),
          ),
    );
  });
  return h(
    "section",
    { class: "setup", "aria-label": "Set up this Mac" },
    h(
      "div",
      { class: "setup-head" },
      h("div", { class: "grow" }, h("h2", {}, "Set up this Mac"), h("p", { class: "meta" }, `macOS asks before ${library.holder} may touch other apps, the screen or the microphone. Allow what your apps need; you can change it later in the Library.`)),
      h(
        "button",
        {
          class: "btn ghost sm",
          onclick: () => {
            prefs.setupDone = true;
            savePrefs();
            actions.render();
          },
        },
        "Done for now",
      ),
    ),
    h("ol", { class: "setup-steps" }, steps),
  );
}

export function problemCount() {
  if (!model.status) return 0;
  if (Date.now() - libraryAt > 10_000) refreshLibrary();
  return problems().length;
}

export function renderHome() {
  if (Date.now() - libraryAt > 10_000) refreshLibrary();
  const list = problems();
  const all = apps();
  const pins = [];
  for (const a of all) {
    for (const sw of a.tile?.switches ?? []) {
      if (prefs.pins.has(pinKey(a.owner, sw.id))) pins.push({ a, sw });
    }
  }
  const widgets = all.filter((a) => a.widget && !prefs.hiddenWidgets.has(a.owner)).sort((x, y) => (x.widget.title || x.label).localeCompare(y.widget.title || y.label));
  return h(
    "div",
    { class: "home" },
    renderSetup(),
    list.length
      ? [
          h("div", { class: "section-title" }, "Needs attention", h("span", { class: "hint" }, String(list.length))),
          h(
            "div",
            { class: "problems" },
            list.map((p) =>
              h(
                "div",
                { class: `problem ${p.level}` },
                h("span", { class: "problem-icon" }, icon(p.level === "err" ? "error" : "alert", { size: 15 })),
                h("span", { class: "grow" }, h("b", {}, p.who), h("span", { class: "problem-what" }, p.what)),
                p.fix,
                h("button", { class: "btn ghost sm", onclick: p.open }, "Details", icon("chevron", { size: 12 })),
              ),
            ),
          ),
        ]
      : all.length
        ? h("div", { class: "all-good" }, icon("check", { size: 16 }), "Every app, sidecar and grant is fine.")
        : h("div", { class: "all-good none" }, icon("apps", { size: 16 }), "No apps yet.", h("button", { class: "btn ghost sm", onclick: () => actions.go("apps") }, "Add one", icon("chevron", { size: 12 }))),
    pins.length ? h("div", { class: "section-title" }, "Pinned") : null,
    pins.length
      ? h(
          "div",
          { class: "pins" },
          pins.map(({ a, sw }) =>
            h(
              "div",
              { class: "pin-card" },
              h("div", { class: "grow" }, h("div", {}, sw.label || sw.id), h("button", { class: "linkish", onclick: () => openApp(a.key, "controls") }, a.label)),
              toggle(sw.on, (on) => actions.send({ cmd: "ui-action", action: { kind: "switch", owner: a.owner, id: sw.id, on } }), sw.label || sw.id),
            ),
          ),
        )
      : null,
    h("div", { class: "section-title" }, "Widgets"),
    widgets.length
      ? h(
          "div",
          { class: "grid" },
          widgets.map((a) =>
            h(
              "article",
              { class: "tile widget" },
              h("div", { class: "tile-head" }, h("h3", {}, a.widget.title || a.label), h("button", { class: "linkish", title: `Open ${a.label}`, onclick: () => openApp(a.key, "now") }, a.label, icon("chevron", { size: 12 }))),
              h("dl", { class: "widget-rows" }, a.widget.rows.flatMap((r) => [h("dt", {}, r.label), h("dd", {}, r.value)])),
              a.item.state !== "running" ? badge(a.item.state, a.item.state) : null,
            ),
          ),
        )
      : emptyState({
          icon: "widget",
          title: "No widgets",
          text: "An app shows a value worth a glance here with ui.set-widget. Pin a switch from an app's Controls tab to show it on Home.",
          small: true,
        }),
  );
}
