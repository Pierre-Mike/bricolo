import { icon } from "./icons.js";

// A small element builder. Text always goes in as text, never as HTML,
// because tiles and panels carry strings from plugins.

export function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") el.className = value;
    else if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else if (key in el && typeof value !== "string") el[key] = value;
    else el.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child === undefined || child === null || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

// An on/off switch. `onchange` gets the new value.
export function toggle(on, onchange, label) {
  const input = h("input", {
    type: "checkbox",
    checked: on,
    "aria-label": label,
    onchange: (e) => onchange(e.target.checked),
  });
  return h("label", { class: "switch" }, input, h("span", { class: "slider" }));
}

export function badge(text, kind = "") {
  return h("span", { class: `badge ${kind}` }, text);
}

// Terminal commands to copy, each with what it does, for empty states.
export function commands(rows) {
  return h(
    "dl",
    { class: "commands" },
    rows.flatMap(([cmd, what]) => [h("dt", {}, h("code", {}, cmd)), h("dd", {}, what)]),
  );
}

// An empty state: an icon, a title, one line of help and an optional action.
// `small` fits inside a drawer tab or a section.
export function emptyState({ icon: name = "inbox", title, text, action, small = false }) {
  return h(
    "div",
    { class: small ? "empty small" : "empty" },
    h("div", { class: "empty-icon" }, icon(name, { size: small ? 18 : 22 })),
    title ? h("h2", {}, title) : null,
    text ? h("p", {}, text) : null,
    action ?? null,
  );
}
