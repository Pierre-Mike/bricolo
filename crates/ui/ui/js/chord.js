// Hotkeys: how a chord looks, and the one control that changes it. Every
// card that shows a hotkey uses `hotkeyChip`, so changing a shortcut works
// the same way for every plugin and script: click the chip, press the new
// chord. Esc cancels; the reset button puts back the chord the app asked for.

import { h } from "./dom.js";
import { icon } from "./icons.js";

const SYMBOLS = {
  cmd: "⌘",
  command: "⌘",
  alt: "⌥",
  option: "⌥",
  opt: "⌥",
  ctrl: "⌃",
  control: "⌃",
  shift: "⇧",
  left: "←",
  right: "→",
  up: "↑",
  down: "↓",
  enter: "↩",
  return: "↩",
  space: "Space",
  escape: "Esc",
  tab: "⇥",
  delete: "⌫",
  fn: "fn",
};

// Modifiers in the order macOS menus show them: ⌃ ⌥ ⇧ ⌘.
const ORDER = ["⌃", "⌥", "⇧", "⌘"];

// "cmd+alt+h" as "⌥⌘H". "cmd++" names the + key.
export function chordLabel(text) {
  const parts = text.endsWith("++") ? [...text.slice(0, -2).split("+"), "+"] : text.split("+");
  const symbols = parts.map((part) => SYMBOLS[part.toLowerCase()] ?? part.toUpperCase());
  const mods = symbols.filter((s) => ORDER.includes(s)).sort((a, b) => ORDER.indexOf(a) - ORDER.indexOf(b));
  return [...mods, ...symbols.filter((s) => !ORDER.includes(s))].join("");
}

// A hotkey id as a person reads it: numbered ids like "h2" say nothing,
// so they read "Shortcut 2"; any other id is its own name.
export function shortcutName(id) {
  const m = /^h([0-9]+)$/.exec(id);
  return m ? `Shortcut ${m[1]}` : id;
}

// KeyboardEvent.code to the key names the node's chords use.
const NAMED = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  Space: "space",
  Enter: "enter",
  NumpadEnter: "enter",
  Tab: "tab",
  Backspace: "delete",
  Delete: "forwarddelete",
  Home: "home",
  End: "end",
  PageUp: "pageup",
  PageDown: "pagedown",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backquote: "`",
};

function keyName(code) {
  if (NAMED[code]) return NAMED[code];
  let m = /^Key([A-Z])$/.exec(code);
  if (m) return m[1].toLowerCase();
  m = /^(?:Digit|Numpad)([0-9])$/.exec(code);
  if (m) return m[1];
  m = /^F([0-9]{1,2})$/.exec(code);
  if (m && Number(m[1]) >= 1 && Number(m[1]) <= 20) return `f${m[1]}`;
  return null;
}

const MODIFIER_CODES = /^(Meta|OS|Control|Alt|Shift|CapsLock|Fn)/;

// The chord a key press makes, like "ctrl+alt+d", or a reason it makes none.
export function chordFromEvent(e) {
  if (MODIFIER_CODES.test(e.code)) return { pending: true };
  const key = keyName(e.code);
  if (!key) return { error: `bricolo cannot bind ${e.code || e.key}` };
  const mods = [];
  if (e.ctrlKey) mods.push("ctrl");
  if (e.altKey) mods.push("alt");
  if (e.shiftKey) mods.push("shift");
  if (e.metaKey) mods.push("cmd");
  const bare = !mods.length || (mods.length === 1 && mods[0] === "shift");
  if (bare && !/^f[0-9]+$/.test(key)) {
    return { error: "Add cmd, ctrl or alt, or use an F key" };
  }
  return { chord: [...mods, key].join("+") };
}

// The chip being recorded, if any. Module state, so a re-render of the
// view while you press keys keeps the chip waiting.
let recording = null;
// The last refusal per chip, shown under the cards until the next try.
const refusals = new Map();
let redraw = () => {};

const keyOf = (owner, id) => `${owner}\u0000${id}`;

export function isRecording() {
  return recording !== null;
}

// `redraw` re-renders the view; `send` sends a control request and answers
// the node's reply.
export function setupHotkeys({ redraw: r, send }) {
  redraw = r;
  // Capture phase: the hub's own Esc handler must not see these keys.
  window.addEventListener(
    "keydown",
    async (e) => {
      if (!recording) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      const target = recording;
      if (e.key === "Escape" && !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey) {
        recording = null;
        redraw();
        return;
      }
      const result = chordFromEvent(e);
      if (result.pending) return;
      if (result.error) {
        refusals.set(target.key, result.error);
        redraw();
        return;
      }
      recording = null;
      refusals.delete(target.key);
      redraw();
      await change(send, target, result.chord);
    },
    true,
  );
  // A click anywhere else stops recording.
  window.addEventListener(
    "mousedown",
    (e) => {
      if (recording && !e.target.closest?.(".chip.recording")) {
        recording = null;
        redraw();
      }
    },
    true,
  );
}

async function change(send, target, chord) {
  const answer = await send({ cmd: "set-hotkey", ...target.request, id: target.id, chord });
  if (answer?.error) refusals.set(target.key, answer.error);
  else refusals.delete(target.key);
  redraw();
}

// One hotkey: `kind` is "plugin" or "script", `name` the plugin or script,
// `chord` what is bound now, `asked` the chord the app asked for when the
// user moved it (else undefined). Ids like h1 and h2 (scripts, and plugins
// that number their bindings) say nothing, so only the chord shows then.
export function hotkeyChip({ kind, name, id, chord, asked, send }) {
  const owner = kind === "script" ? `${name}.lua` : name;
  const key = keyOf(owner, id);
  const target = { key, id, request: { [kind]: name } };
  const isRecordingThis = recording?.key === key;
  const label = /^h[0-9]+$/.test(id) ? null : id;
  const title = isRecordingThis
    ? "Press the new shortcut. Esc cancels."
    : `${chord}. Click to change.${asked ? ` The app asks for ${asked}.` : ""}`;
  const chip = h(
    "button",
    {
      class: `chip hotkey${isRecordingThis ? " recording" : ""}${asked ? " changed" : ""}`,
      title,
      "aria-label": `Shortcut ${label ?? ""} ${chord}. Click to change.`,
      onclick: () => {
        recording = isRecordingThis ? null : target;
        refusals.delete(key);
        redraw();
      },
    },
    h("kbd", {}, isRecordingThis ? "Press keys…" : chordLabel(chord)),
    label,
  );
  const reset = asked
    ? h(
        "button",
        {
          class: "chip-reset",
          title: `Back to ${chordLabel(asked)}`,
          "aria-label": `Back to ${asked}`,
          onclick: () => change(send, target, null),
        },
        icon("reset", { size: 13 }),
      )
    : null;
  return h("span", { class: "hotkey-wrap" }, chip, reset);
}

// The refusals for this plugin's or script's chips, to show in its card.
export function hotkeyErrors(kind, name) {
  const owner = kind === "script" ? `${name}.lua` : name;
  const prefix = `${owner}\u0000`;
  return [...refusals.entries()].filter(([k]) => k.startsWith(prefix)).map(([, message]) => message);
}
