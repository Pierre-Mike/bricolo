// What the hub knows and remembers, shared by its modules: the node's last
// report (`model`), where the user is (`view`), what they pinned (`prefs`,
// kept in localStorage) and the calls hub.js wires in (`actions`).

export const model = {
  connected: false,
  reason: "",
  status: null,
  statusAt: 0,
  ui: { tiles: {}, widgets: {}, panels: [] },
  logs: [],
  canStart: false,
};

export const view = {
  current: "home",
  // The app whose drawer is open, as "plugin:<name>" or "script:<name>".
  open: null,
  tab: "now",
  filter: "",
  kind: "all",
  // Per plugin: the last `settings` answer, unsaved edits and the last error.
  settings: {},
  drafts: {},
  settingsError: {},
  // Plugins whose Advanced settings are unfolded.
  advancedOpen: new Set(),
};

// Set by hub.js: send (errors go to the logs), quietly (errors come back),
// render, and toast.
export const actions = {};

const PREFS = "bricolo-hub-prefs";

function loadPrefs() {
  try {
    const saved = JSON.parse(localStorage.getItem(PREFS) ?? "{}");
    return {
      pins: new Set(saved.pins ?? []),
      hiddenWidgets: new Set(saved.hiddenWidgets ?? []),
      setupDone: saved.setupDone === true,
    };
  } catch {
    return { pins: new Set(), hiddenWidgets: new Set(), setupDone: false };
  }
}

// pins: "<owner>\u0000<switch id>"; hiddenWidgets: owners whose widget
// stays off Home; setupDone: the user closed Home's setup walkthrough.
export const prefs = loadPrefs();

export function savePrefs() {
  localStorage.setItem(
    PREFS,
    JSON.stringify({ pins: [...prefs.pins], hiddenWidgets: [...prefs.hiddenWidgets], setupDone: prefs.setupDone }),
  );
}

export const pinKey = (owner, id) => `${owner}\u0000${id}`;

// Every plugin and script as one list of apps.
export function apps() {
  const s = model.status;
  if (!s) return [];
  const list = [
    ...(s.plugins ?? []).map((item) => ({ kind: "plugin", item, owner: item.name })),
    ...(s.scripts ?? []).map((item) => ({ kind: "script", item, owner: `${item.name}.lua` })),
  ];
  return list.map((a) => ({
    ...a,
    key: `${a.kind}:${a.item.name}`,
    label: a.kind === "script" ? `${a.item.name}.lua` : a.item.name,
    tile: a.item.state === "disabled" ? null : model.ui.tiles?.[a.owner] ?? null,
    widget: model.ui.widgets?.[a.owner] ?? null,
  }));
}

export const findApp = (key) => apps().find((a) => a.key === key);

// The switch that stands for the whole app: the only one its controls have.
export const mainSwitch = (tile) => (tile?.switches.length === 1 ? tile.switches[0] : null);

// "ok", "warn", "err" or "off", for the dot next to an app's name.
export function health(a) {
  const state = a.item.state;
  if (state === "disabled" || state === "stopped") return "off";
  if (state !== "running") return "err";
  return a.item.error ? "warn" : "ok";
}
