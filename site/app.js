// The hero mesh: devices that pass calls to each other, plus copy buttons.

const NAVY = "#1b2761";
const LINE = "#323a8f";
const BRIGHT = "#3b55d4";
const ORANGE = "#f26822";
const WHITE = "#ffffff";

const devices = [
  { id: "laptop", label: "laptop", x: 0.18, y: 0.3 },
  { id: "desktop", label: "desktop · GPU", x: 0.78, y: 0.22 },
  { id: "server", label: "home server", x: 0.82, y: 0.74 },
  { id: "mac", label: "kitchen Mac", x: 0.3, y: 0.8 },
  { id: "pi", label: "Raspberry Pi", x: 0.52, y: 0.5 },
];

const links = [
  ["laptop", "desktop"], ["laptop", "pi"], ["laptop", "mac"],
  ["desktop", "server"], ["desktop", "pi"], ["server", "pi"],
  ["server", "mac"], ["mac", "pi"],
];

// Each call travels from one device to another and back with a result.
const calls = [
  { from: "laptop", to: "desktop", cap: "speech" },
  { from: "mac", to: "server", cap: "disk" },
  { from: "laptop", to: "mac", cap: "tts" },
  { from: "pi", to: "desktop", cap: "http" },
  { from: "server", to: "laptop", cap: "clipboard" },
  { from: "desktop", to: "pi", cap: "shell" },
];

function startMesh() {
  const canvas = document.getElementById("mesh");
  const caption = document.getElementById("mesh-caption");
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const byId = Object.fromEntries(devices.map((d) => [d.id, d]));
  let w = 0;
  let h = 0;

  function resize() {
    const dpr = window.devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    w = rect.width;
    h = rect.height;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  resize();
  window.addEventListener("resize", resize);

  const pos = (d) => [d.x * w, d.y * h];
  const pulses = [];
  let next = 0;
  let lastSpawn = -Infinity;

  function spawn(now) {
    const c = calls[next % calls.length];
    next += 1;
    pulses.push({ ...c, start: now, active: new Set([c.from, c.to]) });
    caption.textContent = `${byId[c.from].label} → ${byId[c.to].label} · ${c.cap}`;
  }

  function draw(now) {
    ctx.clearRect(0, 0, w, h);
    if (now - lastSpawn > 1400) {
      spawn(now);
      lastSpawn = now;
    }

    const lit = new Set();
    for (const p of pulses) {
      const t = (now - p.start) / 1800;
      if (t < 1) p.active.forEach((id) => lit.add(id));
    }

    ctx.lineWidth = 1.5;
    for (const [a, b] of links) {
      const [ax, ay] = pos(byId[a]);
      const [bx, by] = pos(byId[b]);
      ctx.strokeStyle = LINE;
      ctx.beginPath();
      ctx.moveTo(ax, ay);
      ctx.lineTo(bx, by);
      ctx.stroke();
    }

    for (let i = pulses.length - 1; i >= 0; i -= 1) {
      const p = pulses[i];
      const t = (now - p.start) / 1800;
      if (t >= 1) {
        pulses.splice(i, 1);
        continue;
      }
      const [fx, fy] = pos(byId[p.from]);
      const [tx, ty] = pos(byId[p.to]);
      // Out on the first half, back with the result on the second.
      const k = t < 0.5 ? t * 2 : 2 - t * 2;
      const x = fx + (tx - fx) * k;
      const y = fy + (ty - fy) * k;
      ctx.strokeStyle = t < 0.5 ? ORANGE : BRIGHT;
      ctx.lineWidth = 2.5;
      ctx.beginPath();
      ctx.moveTo(fx, fy);
      ctx.lineTo(x, y);
      ctx.stroke();
      ctx.fillStyle = t < 0.5 ? ORANGE : WHITE;
      ctx.beginPath();
      ctx.arc(x, y, 5, 0, Math.PI * 2);
      ctx.fill();
      p.x = x;
      p.y = y;
    }

    for (const d of devices) {
      const [x, y] = pos(d);
      const on = lit.has(d.id);
      ctx.fillStyle = NAVY;
      ctx.strokeStyle = on ? ORANGE : WHITE;
      ctx.lineWidth = on ? 3 : 2;
      ctx.beginPath();
      ctx.roundRect(x - 26, y - 18, 52, 36, 6);
      ctx.fill();
      ctx.stroke();
      ctx.fillStyle = WHITE;
      ctx.font = "600 11px ui-monospace, Menlo, monospace";
      ctx.textAlign = "center";
      ctx.fillText("bricolo", x, y + 4);
      ctx.font = "13px 'Segoe UI', Calibri, Arial, sans-serif";
      ctx.fillStyle = on ? WHITE : "#aeb6e6";
      ctx.fillText(d.label, x, y + 36);
      ctx.textAlign = "start";
    }

    // Labels last, so the device boxes never cover them.
    ctx.font = "600 12px ui-monospace, Menlo, monospace";
    ctx.textAlign = "center";
    for (const p of pulses) {
      if (p.x === undefined) continue;
      const label = p.x > w - 60 ? p.x - 30 : p.x;
      ctx.fillStyle = NAVY;
      const width = ctx.measureText(p.cap).width + 8;
      ctx.fillRect(label - width / 2, p.y - 26, width, 16);
      ctx.fillStyle = ORANGE;
      ctx.fillText(p.cap, label, p.y - 14);
    }
    ctx.textAlign = "start";

    if (!reduced) requestAnimationFrame(draw);
  }

  requestAnimationFrame(draw);
}

// Copies the text of the element a button names in data-copy.
function startCopy() {
  for (const button of document.querySelectorAll("[data-copy]")) {
    button.addEventListener("click", async () => {
      const target = document.getElementById(button.dataset.copy);
      if (!target) return;
      try {
        await navigator.clipboard.writeText(target.textContent.trim());
        button.textContent = "Copied";
      } catch {
        button.textContent = "Select and copy";
      }
      setTimeout(() => (button.textContent = "Copy"), 1600);
    });
  }
}

startMesh();
startCopy();
