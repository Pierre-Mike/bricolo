// A widget's body, on Home and in an app's Now tab: its gauges, then its
// rows.
//
// A gauge (ui.set-gauge-widget) is a bar across the card: the label and
// value above it, the note under it. The bar is split into `marks` equal
// segments (the hours of a window, the days of a week) and filled to
// `fill` percent. `now` draws a line across it; when the fill runs past
// the line, the fill turns to the warning color: at that pace the limit
// comes before the end of the window.

import { h } from "./dom.js";

const clamp = (n) => Math.min(100, Math.max(0, Number(n) || 0));

function bar(g) {
  const marks = Math.max(1, Math.min(32, Math.round(g.marks) || 1));
  const fill = clamp(g.fill);
  const now = g.now == null ? null : clamp(g.now);
  const ahead = now !== null && fill > now;
  const label = now === null ? `${fill}%` : `${fill}%, now at ${now}%`;
  const segments = Array.from({ length: marks }, (_, i) => {
    // How much of segment i the fill covers, 0 to 100.
    const part = clamp((fill / 100) * marks * 100 - i * 100);
    const u = h("u");
    u.style.width = `${part}%`;
    return h("i", {}, part > 0 ? u : null);
  });
  const line = now === null ? null : h("b", { class: "now" });
  if (line) line.style.left = `${now}%`;
  return h("div", { class: ahead ? "gauge-bar ahead" : "gauge-bar", role: "img", "aria-label": label }, segments, line);
}

function gauge(g) {
  return h(
    "div",
    { class: "gauge" },
    h("div", { class: "gauge-top" }, h("span", {}, g.label), h("b", {}, g.value)),
    bar(g),
    g.note ? h("p", { class: "gauge-note" }, g.note) : null,
  );
}

export function widgetBody(w) {
  const gauges = w.gauges ?? [];
  return [
    gauges.length ? h("div", { class: "gauges" }, gauges.map(gauge)) : null,
    w.rows?.length ? h("dl", { class: "widget-rows" }, w.rows.flatMap((r) => [h("dt", {}, r.label), h("dd", {}, r.value)])) : null,
  ];
}
