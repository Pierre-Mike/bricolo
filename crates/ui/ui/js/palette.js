// The command palette: cmd+K or / in the hub. It searches every switch,
// button, shortcut and settings tab, and runs the one you pick.

import { openApp } from "./apps.js";
import { chordLabel, shortcutName } from "./chord.js";
import { h } from "./dom.js";
import { icon } from "./icons.js";
import { actions, apps } from "./store.js";

function entries() {
  const out = [];
  for (const a of apps()) {
    out.push({ verb: "Open", icon: a.kind, what: a.label, hint: a.kind, run: () => openApp(a.key) });
    for (const sw of a.tile?.switches ?? []) {
      out.push({
        verb: sw.on ? "Turn off" : "Turn on",
        icon: "power",
        what: sw.label || sw.id,
        hint: a.label,
        run: () => actions.send({ cmd: "ui-action", action: { kind: "switch", owner: a.owner, id: sw.id, on: !sw.on } }),
      });
    }
    for (const b of a.tile?.buttons ?? []) {
      out.push({ verb: "Run", icon: "play", what: b.label || b.id, hint: a.label, run: () => actions.send({ cmd: "ui-action", action: { kind: "button", owner: a.owner, id: b.id } }) });
    }
    if (a.kind === "plugin") {
      for (const [id, chord] of Object.entries(a.item.hotkeys ?? {})) {
        out.push({ verb: "Run", icon: "keyboard", what: shortcutName(id), hint: a.label, chord: chordLabel(chord), run: () => actions.send({ cmd: "fire", plugin: a.item.name, hotkey: id }) });
      }
      out.push({ verb: "Open", icon: "sliders", what: "Settings", hint: a.label, run: () => openApp(a.key, "settings") });
    }
  }
  const views = [["home", "Home", "home"], ["apps", "Apps", "apps"], ["sidecars", "Sidecars", "server"], ["library", "Library", "library"], ["mesh", "Mesh and routes", "network"], ["logs", "Logs", "terminal"]];
  views.forEach(([view, label, glyph], i) => {
    out.push({ verb: "Go to", icon: glyph, what: label, hint: "view", chord: `⌘${i + 1}`, run: () => actions.go(view) });
  });
  return out;
}

export const isPaletteOpen = () => document.querySelector(".palette") !== null;

export function openPalette() {
  if (isPaletteOpen()) return;
  const all = entries();
  let selected = 0;
  let shown = all;
  const list = h("ul", { role: "listbox" });
  const input = h("input", { type: "text", placeholder: "Type an action, an app or a view…", "aria-label": "Search actions" });
  const close = () => {
    scrim.remove();
    box.remove();
  };
  const run = (entry) => {
    close();
    entry.run();
  };
  const draw = () => {
    const words = input.value.toLowerCase().split(/\s+/).filter(Boolean);
    shown = all.filter((e) => words.every((w) => `${e.verb} ${e.what} ${e.hint}`.toLowerCase().includes(w))).slice(0, 50);
    selected = Math.min(selected, Math.max(0, shown.length - 1));
    if (!shown.length) {
      list.replaceChildren(h("li", { class: "none" }, `Nothing matches “${input.value}”.`));
      return;
    }
    list.replaceChildren(
      ...shown.map((e, i) =>
        h(
          "li",
          {
            class: i === selected ? "sel" : "",
            role: "option",
            "aria-selected": String(i === selected),
            onclick: () => run(e),
            onmousemove: (e) => select(i, e),
          },
          h("span", { class: "p-icon" }, icon(e.icon ?? "chevron", { size: 15 })),
          h("span", { class: "grow" }, h("span", { class: "p-what" }, e.what), e.hint ? h("span", { class: "meta p-hint" }, e.hint) : null),
          e.chord ? h("kbd", {}, e.chord) : null,
          h("span", { class: "p-verb" }, e.verb),
        ),
      ),
    );
    list.children[selected]?.scrollIntoView({ block: "nearest" });
  };
  // The pointer moves the selection without rebuilding the list.
  // WebKit sends a mousemove after a scroll with the pointer still; only a
  // pointer that really moved takes the selection.
  let pointer = "";
  const select = (i, e) => {
    const at = e ? `${e.clientX},${e.clientY}` : "";
    if (e && at === pointer) return;
    pointer = at;
    if (i === selected) return;
    list.children[selected]?.classList.remove("sel");
    list.children[selected]?.setAttribute("aria-selected", "false");
    selected = i;
    list.children[selected]?.classList.add("sel");
    list.children[selected]?.setAttribute("aria-selected", "true");
  };
  input.addEventListener("input", () => {
    selected = 0;
    draw();
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") {
      selected = Math.min(selected + 1, shown.length - 1);
      draw();
      e.preventDefault();
    } else if (e.key === "ArrowUp") {
      selected = Math.max(selected - 1, 0);
      draw();
      e.preventDefault();
    } else if (e.key === "Enter" && shown[selected]) {
      run(shown[selected]);
    } else if (e.key === "Escape") {
      close();
      e.stopPropagation();
    }
  });
  const scrim = h("div", { class: "palette-scrim", onclick: close });
  const box = h(
    "div",
    { class: "palette", role: "dialog", "aria-label": "Search actions" },
    h("div", { class: "palette-input" }, icon("search", { size: 16 }), input),
    list,
    h(
      "div",
      { class: "palette-foot" },
      h("span", {}, h("kbd", {}, "↑"), h("kbd", {}, "↓"), " move"),
      h("span", {}, h("kbd", {}, "↩"), " run"),
      h("span", {}, h("kbd", {}, "esc"), " close"),
    ),
  );
  document.body.append(scrim, box);
  draw();
  input.focus();
}
