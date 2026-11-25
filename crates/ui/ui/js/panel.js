// One floating panel window. bricolo-ui makes one window per kind (its label
// is `panel-<kind>`) and sends `{kind, panel}`, with null to clear it.

import { h } from "./dom.js";
import { invoke, listen, request, windowLabel } from "./tauri.js";

const kind = windowLabel().replace(/^panel-/, "");
const root = document.getElementById("root");
root.className = kind;

let shown = null;
let selected = 0;

const act = (action) => request({ cmd: "ui-action", action }).catch(() => {});

// Answers a choice list or review: hides this panel first, so a paste the
// plugin sends in reply reaches the app below, not this panel.
const answer = (action) =>
  invoke("dismiss_panel", { kind })
    .catch(() => {})
    .then(() => act(action));

// Key hints for a panel's footer: each key drawn as a key, then what it does.
function hints(pairs) {
  return h("span", { class: "hints" }, pairs.map(([key, what]) => h("span", {}, h("kbd", {}, key), what)));
}

function titled(panel, ...body) {
  return h("div", { class: "box" }, panel.title ? h("div", { class: "title" }, panel.title) : null, ...body);
}

function caption(panel) {
  return titled(panel, h("div", { class: "text" }, panel.text));
}

function toast(panel) {
  return titled(panel, h("div", { class: "text" }, panel.text));
}

function pick(index) {
  if (!shown) return;
  answer({ kind: "choice", panel: shown.id, index });
}

function choiceList(panel) {
  const items = panel.choices.map((choice, i) =>
    h(
      "li",
      {
        class: i === selected ? "selected" : "",
        onmousemove: () => select(i),
        onclick: () => pick(i),
      },
      h("span", { class: "key" }, i < 9 ? i + 1 : ""),
      h("span", {}, choice),
    ),
  );
  return titled(
    panel,
    panel.text ? h("div", {}, panel.text) : null,
    h("ul", {}, items),
    h("div", { class: "foot" }, hints([["↑↓", "move"], ["↩", "pick"], ["1–9", "pick by number"], ["esc", "dismiss"]])),
  );
}

function select(i) {
  if (i === selected) return;
  selected = i;
  root.querySelectorAll("li").forEach((li, n) => li.classList.toggle("selected", n === selected));
  root.querySelectorAll("li")[selected]?.scrollIntoView({ block: "nearest" });
}

function review(panel) {
  const text = h("textarea", { spellcheck: true }, panel.text);
  const done = (accepted) => answer({ kind: "review", panel: panel.id, accepted, text: text.value });
  text.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      done(true);
    } else if (e.key === "Escape") {
      e.preventDefault();
      done(false);
    }
  });
  requestAnimationFrame(() => {
    text.focus();
    text.setSelectionRange(text.value.length, text.value.length);
  });
  return titled(
    panel,
    text,
    h(
      "div",
      { class: "foot" },
      hints([["↩", "send"], ["⇧↩", "new line"], ["esc", "cancel"]]),
      h(
        "div",
        { class: "buttons" },
        h("button", { onclick: () => done(false) }, "Cancel"),
        h("button", { class: "primary", onclick: () => done(true) }, "Send"),
      ),
    ),
  );
}

const RENDER = { caption, toast, "choice-list": choiceList, review };

function show(panel) {
  // A review being edited keeps its text when the same panel arrives again.
  const same = shown && panel && shown.id === panel.id && shown.seq === panel.seq;
  shown = panel;
  if (same) return;
  if (!panel) {
    root.replaceChildren();
    return;
  }
  selected = 0;
  root.replaceChildren(RENDER[kind](panel));
}

document.addEventListener("keydown", (e) => {
  if (!shown || kind !== "choice-list") return;
  const count = shown.choices.length;
  if (e.key === "ArrowDown") select((selected + 1) % count);
  else if (e.key === "ArrowUp") select((selected + count - 1) % count);
  else if (e.key === "Enter") pick(selected);
  else if (e.key === "Escape") pick(null);
  else if (/^[1-9]$/.test(e.key) && Number(e.key) <= count) pick(Number(e.key) - 1);
  else return;
  e.preventDefault();
});

// Every panel window hears every "panel" event; keep this kind's.
listen("panel", (e) => {
  if (e.kind === kind) show(e.panel);
});
invoke("panel_current", { kind }).then(show);
