// The Library view: the standard library this node offers, who may call
// each capability, and the macOS grants the node holds. The node answers
// {"cmd":"library"}; Ask and Revoke… send {"cmd":"permissions",...} and
// {"cmd":"open-settings",...}. macOS lets no program remove its own grant,
// so Revoke… opens the pane where you turn it off.

import { badge, h } from "./dom.js";
import { actions, prefs, savePrefs } from "./store.js";

// The grants the recording badge's sensors use, for "in use now".
const SENSOR_GRANT = { mic: "microphone", camera: "camera", screen: "screen_recording" };

// Grant state to the hub's dot colours; "not asked" stays grey.
const DOT = { granted: "ok", missing: "err" };

const state = {
  answer: null,
  error: "",
  loading: false,
  filter: "",
};

// Parts kept across redraws, so the filter box keeps its focus and text.
let root = null;
let strip = null;
let list = null;

// `deps`: { request, send, status, connected, openCard }.
let deps = null;

export function setupLibrary(d) {
  deps = d;
  // Coming back from System Settings: check again.
  window.addEventListener("focus", () => {
    if (root?.isConnected) refresh();
  });
}

export async function refresh() {
  if (!deps || state.loading) return;
  state.loading = true;
  try {
    const answer = await deps.request({ cmd: "library" });
    state.answer = answer?.error ? null : answer;
    state.error = answer?.error ?? "";
    // A node started before the Library existed doesn't know the request.
    if (state.error.includes("unknown variant `library`")) {
      state.error = "The running node is older than this hub. Restart the node (bricolo service stop, then bricolo service start) to see the Library.";
    }
  } catch (e) {
    state.error = String(e);
  }
  state.loading = false;
  update();
}

// The whole view. Called when the user switches to it.
export function renderLibrary() {
  const search = h("input", {
    type: "search",
    placeholder: "Filter capabilities",
    value: state.filter,
    oninput: (e) => {
      state.filter = e.target.value;
      update();
    },
  });
  strip = h("div", { class: "grants" });
  list = h("div", { class: "capabilities" });
  root = h(
    "div",
    { class: "library" },
    strip,
    h(
      "div",
      { class: "toolbar" },
      h("div", { class: "section-title" }, "Capabilities"),
      search,
      h("button", { class: "btn sm", onclick: () => refresh() }, "Refresh"),
    ),
    list,
  );
  update();
  refresh();
  return root;
}

// Redraws the strip and the list, not the filter box. Called on each
// status tick, for "in use now".
export function update() {
  if (!root) return;
  strip.replaceChildren(...renderGrants());
  list.replaceChildren(...renderCapabilities());
}

function inUse() {
  const used = new Map();
  for (const s of deps?.status()?.active_sensors ?? []) {
    const grant = SENSOR_GRANT[s.sensor];
    if (!grant) continue;
    if (!used.has(grant)) used.set(grant, []);
    used.get(grant).push(s.plugin);
  }
  return used;
}

function renderGrants() {
  const a = state.answer;
  if (!a) {
    const text = state.error || (deps?.connected() ? "Asking the node…" : "Start the node to see what it holds.");
    return [h("div", { class: "section-title" }, h("span", { class: "keep-case" }, "macOS"), " access"), h("p", { class: "meta" }, text)];
  }
  if (a.grants.every((g) => g.state === "n/a")) {
    return [h("div", { class: "section-title" }, "OS access"), h("p", { class: "meta" }, "No OS grants apply here.")];
  }
  const used = inUse();
  const terminal = a.holder.startsWith("your terminal");
  const rows = a.grants.map((g) => {
    const users = used.get(g.name) ?? [];
    let action;
    if (g.state === "granted") {
      action = h(
        "button",
        {
          class: "btn sm",
          title: `Opens System Settings. Turn off ${a.holder} in the list.`,
          onclick: () => deps.send({ cmd: "open-settings", grant: g.name }),
        },
        "Revoke…",
      );
    } else {
      action = h(
        "button",
        {
          class: "btn primary sm",
          onclick: async () => {
            await deps.send({ cmd: "permissions", request: true, grant: g.name });
            await deps.send({ cmd: "open-settings", grant: g.name });
            refresh();
          },
        },
        "Ask macOS",
      );
    }
    return h(
      "tr",
      {},
      h("td", {}, h("span", { class: `dot ${DOT[g.state] ?? ""}` }), h("strong", {}, g.label)),
      h("td", {}, badge(stateText(g.state), `grant-${g.state}`)),
      h("td", { class: "meta" }, users.length ? `in use now: ${users.join(", ")}` : ""),
      h("td", { class: "actions" }, action),
    );
  });
  return [
    h("div", { class: "section-title" }, h("span", { class: "keep-case" }, "macOS"), " access"),
    h(
      "p",
      { class: "meta" },
      `These grants belong to ${a.holder}. Revoke… opens System Settings, where you turn one off. `,
      h(
        "button",
        {
          class: "linkish",
          onclick: () => {
            prefs.setupDone = false;
            savePrefs();
            actions.go("home");
          },
        },
        "Show the setup walkthrough on Home",
      ),
    ),
    terminal
      ? h(
          "p",
          { class: "warn" },
          "bricolo runs from a terminal, so macOS gives these grants to the terminal. Turning one off also takes it from every program you run there.",
        )
      : null,
    h("table", { class: "grant-table" }, h("tbody", {}, rows)),
  ].filter(Boolean);
}

function stateText(s) {
  return { granted: "granted", missing: "missing", "not-asked": "not asked" }[s] ?? s;
}

function renderCapabilities() {
  const a = state.answer;
  if (!a) return [];
  const grants = Object.fromEntries(a.grants.map((g) => [g.name, g]));
  const needle = state.filter.trim().toLowerCase();
  const caps = a.capabilities.filter(
    (c) => !needle || c.name.includes(needle) || c.summary.toLowerCase().includes(needle),
  );
  if (!caps.length) return [h("p", { class: "meta" }, "No capability matches.")];
  const scripts = a.scripts ?? [];
  const note = h(
    "p",
    { class: "meta" },
    "Plugins may call only what you approved at install. Scripts get every capability: remove a script to take it away.",
  );
  return [note, ...caps.map((c) => {
    const grant = c.needs ? grants[c.needs] : null;
    const missing = grant && (grant.state === "missing" || grant.state === "not-asked");
    const users = [
      scripts.length ? h("span", { class: "chip-static" }, `all scripts (${scripts.length})`) : null,
      ...c.plugins.map((p) =>
        h("button", { class: "link", title: "Open its card", onclick: () => deps.openCard("plugin", p) }, p),
      ),
    ].filter(Boolean);
    return h(
      "article",
      { class: `capability ${missing ? "missing" : ""}` },
      h(
        "div",
        { class: "cap-head" },
        h("h3", { class: "mono" }, c.name),
        grant
          ? badge(`needs ${grant.label}${c.note ? ` (${c.note})` : ""}`, missing ? "grant-missing" : "")
          : null,
      ),
      h("p", {}, c.summary),
      h(
        "div",
        { class: "used-by" },
        h("span", { class: "meta" }, "Used by:"),
        users.length ? users : h("span", { class: "meta" }, "nothing installed"),
      ),
    );
  })];
}
