// Tests for server.mjs: `node --test` in this folder.

import assert from "node:assert/strict";
import { createServer } from "node:http";
import { after, before, test } from "node:test";

import { handler, processText, warmUp } from "./server.mjs";

let server;
let base;

before(async () => {
  await warmUp();
  server = createServer(handler);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  base = `http://127.0.0.1:${server.address().port}`;
});

after(() => server.close());

test("process upper-cases", async () => {
  assert.equal(await processText("abc"), "ABC");
});

test("health is ready", async () => {
  const r = await fetch(`${base}/health`);
  assert.deepEqual(await r.json(), { ready: true });
});

test("POST /process answers text", async () => {
  const r = await fetch(`${base}/process`, { method: "POST", body: "héllo" });
  assert.equal(r.status, 200);
  assert.equal(await r.text(), "HÉLLO");
});
