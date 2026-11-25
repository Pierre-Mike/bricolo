// __NAME__: a sidecar for bricolo.
//
// A small HTTP server on 127.0.0.1 that the bricolo node starts, health-checks
// and restarts (`bricolo sidecar add`). Built-in modules only, no npm install.
//
//   GET  /health   {"ready": true} once warmUp() has finished; until then
//                  {"ready": false}, and the node shows "starting".
//   POST /process  the request body as UTF-8 text in, text out. The
//                  example upper-cases it. Errors answer 4xx or 5xx with
//                  the reason as the body.
//
// Put your work in warmUp() (load a model once) and process(). Run it by
// hand with `node server.mjs`; the port is __PORT__, or $SIDECAR_PORT.

import { createServer } from "node:http";
import { pathToFileURL } from "node:url";

const HOST = "127.0.0.1";
const PORT = Number(process.env.SIDECAR_PORT ?? __PORT__);
const MAX_BODY = 8 * 1024 * 1024;

let ready = false;

/** Loads what process() needs, once. /health says "ready": false meanwhile. */
export async function warmUp() {
  ready = true;
}

/** The work: text in, text out. */
export async function processText(text) {
  return text.toUpperCase();
}

function reply(res, status, body, type = "text/plain; charset=utf-8") {
  const data = Buffer.from(body, "utf8");
  res.writeHead(status, { "Content-Type": type, "Content-Length": data.length });
  res.end(data);
}

export function handler(req, res) {
  if (req.method === "GET" && req.url === "/health") {
    return reply(res, 200, JSON.stringify({ ready }), "application/json");
  }
  if (req.method !== "POST" || req.url !== "/process") {
    return reply(res, 404, "no such endpoint\n");
  }
  if (!ready) return reply(res, 503, "still warming up\n");
  const chunks = [];
  let size = 0;
  req.on("data", (chunk) => {
    size += chunk.length;
    if (size > MAX_BODY) {
      reply(res, 413, "the body is larger than 8 MiB\n");
      req.destroy();
    } else {
      chunks.push(chunk);
    }
  });
  req.on("end", async () => {
    try {
      reply(res, 200, await processText(Buffer.concat(chunks).toString("utf8")));
    } catch (e) {
      // The reason goes back to the caller.
      reply(res, 500, `${e?.name ?? "Error"}: ${e?.message ?? e}\n`);
    }
  });
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const server = createServer(handler);
  server.listen(PORT, HOST, () => {
    console.log(`__NAME__ listening on http://${HOST}:${PORT}`);
    warmUp().catch((e) => console.error("warm-up failed:", e));
  });
  process.on("SIGTERM", () => server.close(() => process.exit(0)));
}
