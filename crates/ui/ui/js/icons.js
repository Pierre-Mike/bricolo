// A small inline icon set, drawn from Lucide (https://lucide.dev, ISC
// licence). Icons are built with createElementNS, never innerHTML, and use
// currentColor, so they take the colour of the text around them.
//
// Each icon is a list of shapes: a string is a path's `d`, an object is
// another element with its attributes.

const rect = (x, y, width, height, rx = 2) => ({ t: "rect", x, y, width, height, rx });
const circle = (cx, cy, r) => ({ t: "circle", cx, cy, r });

const ICONS = {
  home: [
    "M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8",
    "M3 10a2 2 0 0 1 .709-1.528l7-5.999a2 2 0 0 1 2.582 0l7 5.999A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  ],
  apps: [rect(3, 3, 7, 7, 1), rect(14, 3, 7, 7, 1), rect(14, 14, 7, 7, 1), rect(3, 14, 7, 7, 1)],
  server: [rect(2, 2, 20, 8), rect(2, 14, 20, 8), "M6 6h.01", "M6 18h.01"],
  library: ["m16 6 4 14", "M12 6v14", "M8 8v12", "M4 4v16"],
  network: [
    rect(16, 16, 6, 6, 1),
    rect(2, 16, 6, 6, 1),
    rect(9, 2, 6, 6, 1),
    "M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3",
    "M12 12V8",
  ],
  terminal: ["m4 17 6-6-6-6", "M12 19h8"],
  search: [circle(11, 11, 8), "m21 21-4.3-4.3"],
  x: ["M18 6 6 18", "m6 6 12 12"],
  reset: ["M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8", "M3 3v5h5"],
  reload: [
    "M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8",
    "M21 3v5h-5",
    "M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16",
    "M8 16H3v5",
  ],
  alert: [
    "m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3",
    "M12 9v4",
    "M12 17h.01",
  ],
  error: [circle(12, 12, 10), "M12 8v4", "M12 16h.01"],
  check: [circle(12, 12, 10), "m9 12 2 2 4-4"],
  chevron: ["m9 18 6-6-6-6"],
  play: ["M6 3 20 12 6 21Z"],
  power: ["M12 2v10", "M18.4 6.6a9 9 0 1 1-12.77.04"],
  pin: [
    "M12 17v5",
    "M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z",
  ],
  plugin: [
    "M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z",
    "m3.3 7 8.7 5 8.7-5",
    "M12 22V12",
  ],
  script: [
    "M10 12.5 8 15l2 2.5",
    "m14 12.5 2 2.5-2 2.5",
    "M14 2v4a2 2 0 0 0 2 2h4",
    "M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7z",
  ],
  keyboard: [
    rect(2, 4, 20, 16),
    "M6 8h.01",
    "M10 8h.01",
    "M14 8h.01",
    "M18 8h.01",
    "M8 12h.01",
    "M12 12h.01",
    "M16 12h.01",
    "M7 16h10",
  ],
  activity: ["M22 12h-4l-3 9L9 3l-3 9H2"],
  sliders: ["M4 21v-7", "M4 10V3", "M12 21v-9", "M12 8V3", "M20 21v-5", "M20 12V3", "M2 14h4", "M10 8h4", "M18 16h4"],
  copy: [rect(8, 8, 14, 14), "M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"],
  mic: ["M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z", "M19 10v2a7 7 0 0 1-14 0v-2", "M12 19v3"],
  inbox: [
    "M22 12h-6l-2 3h-4l-2-3H2",
    "M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z",
  ],
  plug: ["M12 22v-5", "M9 8V2", "M15 8V2", "M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z"],
  list: ["M3 12h.01", "M3 18h.01", "M3 6h.01", "M8 12h13", "M8 18h13", "M8 6h13"],
  widget: [rect(3, 3, 18, 18), "M3 9h18", "M9 21V9"],
  info: [circle(12, 12, 10), "M12 16v-4", "M12 8h.01"],
  toggle: [circle(15, 12, 3), rect(2, 5, 20, 14, 7)],
};

const NS = "http://www.w3.org/2000/svg";

// An <svg> for `name`, `size` px square. Unknown names draw nothing.
export function icon(name, { size = 16, cls = "" } = {}) {
  const svg = document.createElementNS(NS, "svg");
  for (const [k, v] of Object.entries({
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    "stroke-width": 1.75,
    "stroke-linecap": "round",
    "stroke-linejoin": "round",
    "aria-hidden": "true",
    class: `icon ${cls}`.trim(),
  })) {
    svg.setAttribute(k, v);
  }
  for (const shape of ICONS[name] ?? []) {
    const el = document.createElementNS(NS, typeof shape === "string" ? "path" : shape.t);
    if (typeof shape === "string") el.setAttribute("d", shape);
    else for (const [k, v] of Object.entries(shape)) if (k !== "t") el.setAttribute(k, v);
    svg.append(el);
  }
  return svg;
}
