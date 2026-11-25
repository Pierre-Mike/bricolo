#!/usr/bin/env python3
"""tailscale-funnel: one public address for this Mac, leased by whoever needs
it, on 127.0.0.1:8795. A port of apps/tailscale_funnel from the Hammerspoon
setup; the tailscale-funnel plugin draws its widget.

The address is a Tailscale Funnel on <machine>.<tailnet>.ts.net. It belongs
to the machine and the tailnet, not to a process, so a voice agent restart,
a bricolo reload and a reboot all leave it alone. This server watches it with two
read-only commands, `tailscale status --json` and `tailscale serve status
--json`, every 30 s while a lease is live and every 5 minutes otherwise.

It never arms the funnel by itself: only POST /arm does, and only the tile
calls it. It never turns the funnel off when the last lease goes.

    POST /lease    {"owner":"voice-agent","mode":"funnel","port":8794,
                    "probe_path":"/phone/ping","probe_expect":"<token>"}
                   takes or renews a lease; it expires 120 s after the last
                   one. 400 for a bad request, 409 for a second owner on
                   another port, 503 while a human has to act.
    GET  /funnel   the same answer, without touching a lease (/tunnel too)
    POST /release  {"owner":"voice-agent"}
    GET  /health   {"ready": true}
    GET  /view     the answer plus what the tile shows: status_line,
                   url_line, setup_line, next_step, steps, webhook_url...
    POST /check    reads tailscale again now; answers like /funnel
    POST /arm      `tailscale funnel --bg --https=443 http://127.0.0.1:<port>`
                   for the leased port. 409 with no lease, 502 if refused
    POST /disarm   `tailscale funnel --https=443 off`

/lease, /funnel, /tunnel, /release and /check also answer a GET with query
parameters. The lease answer always has every key:
    {"status":"ready","url":"https://mac.tail1234.ts.net",
     "hostname":"mac.tail1234.ts.net","generation":1,"error":null,
     "uptime_s":182,"mode":"funnel","port":8794,"funnel":true,"leases":[...]}
`url` is null until the funnel carries the leased port. The port is 8795, or
$SIDECAR_PORT. Standard library only.
"""

import json
import os
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit

import funnel as F

HOST = "127.0.0.1"
PORT = int(os.environ.get("SIDECAR_PORT", str(F.PORT)))
MAX_BODY = 64 * 1024
CLI_TIMEOUT_S = 15
PROBE_TIMEOUT_S = 10
# How long a fresh lease waits for tailscale's answer before replying. The
# voice agent gives up after 5 s.
ASK_WAIT_S = 3


def log(fmt, *args):
    sys.stderr.write((fmt % args if args else fmt) + "\n")
    sys.stderr.flush()


def run_cli(binary, args):
    """One tailscale call -> (exit code, stdout + stderr). Code None when it
    couldn't run or timed out."""
    try:
        done = subprocess.run(
            [binary, *args],
            capture_output=True,
            text=True,
            timeout=CLI_TIMEOUT_S,
            env={**os.environ, **F.CLI_ENV},
            stdin=subprocess.DEVNULL,
        )
    except subprocess.TimeoutExpired:
        return None, "tailscale %s timed out after %d s" % (args[0], CLI_TIMEOUT_S)
    except OSError as e:
        return None, str(e)
    return done.returncode, (done.stdout or "") + (done.stderr or "")


def fetch(url):
    """GET -> (status, body). Status None when nothing answered."""
    try:
        with urllib.request.urlopen(url, timeout=PROBE_TIMEOUT_S) as r:
            return r.status, r.read(65536).decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, ""
    except (urllib.error.URLError, OSError, ValueError):
        return None, ""


def first_line(text):
    for line in (text or "").splitlines():
        if line.strip():
            return line.strip()
    return "no output"


class Funnel:
    """The leases and what tailscale last said. `run`, `exists`, `now` and
    `fetch` are seams: the tests pass fakes, so no tailscale runs."""

    def __init__(self, run=run_cli, exists=os.path.exists, now=time.time, fetch=fetch):
        self.run_cli, self.exists, self.now, self.fetch = run, exists, now, fetch
        self.lock = threading.RLock()
        self.leases = {}  # owner -> {"expires", "taken", "req"}
        self.bin = None
        # Observations: what the last two read-only commands said.
        self.status_info = None
        self.serve_info = None
        self.cli_err = None
        # What settle() derives from them.
        self.hostname = None
        self.url = None
        self.funnel = False
        self.generation = 0
        self.err = None
        self.fatal = False
        self.mode = None
        self.port = None
        self.owner = None
        self.started_at = None
        self.pending = False
        self.misses = 0
        self.seq = 0
        self.ticks = 0
        self.was_up = False
        self.runs = []  # every argument list run, for the tests

    # ── State ────────────────────────────────────────────────────────────
    def raw(self):
        return {
            "fatal": self.fatal, "mode": self.mode, "url": self.url, "hostname": self.hostname,
            "funnel": self.funnel, "pending": self.pending, "generation": self.generation,
            "err": self.err, "started_at": self.started_at, "port": self.port, "leases": self.leases,
        }

    def state(self):
        with self.lock:
            return F.snapshot(self.raw(), self.now())

    def wanted(self):
        req, _ = F.primary(self.leases, self.now())
        return req is not None and req["mode"] == "funnel"

    def find_binary(self):
        self.bin = F.find_bin(self.exists)
        return self.bin

    # ── Deciding ─────────────────────────────────────────────────────────
    def settle(self):
        """The one place that turns the observations into what the tile shows
        and what a caller is told. Call it with the lock held."""
        now = self.now()
        req, owner = F.primary(self.leases, now)
        self.owner = owner
        self.mode = req["mode"] if req else None
        self.port = req["port"] if req and req["mode"] == "funnel" else None
        if self.status_info:
            self.hostname = self.status_info["hostname"]

        # An `off` lease: its owner has a public URL of its own.
        if self.mode == "off":
            self.url, self.fatal, self.err = req["public_url"], False, None
            return

        # tailscaled keeps the route while Tailscale is stopped, so a route
        # only counts as a live funnel while the backend runs.
        running = bool(self.status_info and self.status_info.get("backend") == "Running")
        up = bool(running and self.serve_info and self.serve_info.get("on"))
        if up != self.was_up:
            if up:
                self.generation, self.started_at, self.misses = self.generation + 1, now, 0
                log("[funnel] on at %s carrying %s (generation %d)", F.host_url(self.serve_info.get("host")) or F.host_url(self.hostname), self.serve_info.get("target"), self.generation)
            else:
                self.started_at = None
                log("[funnel] off")
            self.was_up = up
        self.funnel = up
        live = (F.host_url(self.serve_info.get("host")) or F.host_url(self.hostname)) if up else None

        problem = (
            (F.NOT_INSTALLED if not self.bin and self.wanted() else None)
            or self.cli_err
            or (F.status_problem(self.status_info) if self.status_info else None)
            or F.port_problem(self.serve_info, self.port)
            or (F.PROBE_DEAD if up and F.probe_failed(self.misses) else None)
        )
        if problem:
            self.fatal, self.err = True, problem
            # The address still exists when the only complaint is that it
            # stopped answering; every other fault means there is none.
            self.url = live if problem == F.PROBE_DEAD else None
            return
        self.fatal = False
        self.url = live
        # Not a fault, but the caller still needs to know why there is no URL.
        self.err = F.FUNNEL_OFF if (not up and req) else None

    def _run(self, args):
        self.runs.append(list(args))
        code, out = self.run_cli(self.bin, args)
        if code != 0:
            log("[tailscale] %s -> %s: %s", args[0], code, first_line(out))
        return code, out

    # ── Reading tailscale ────────────────────────────────────────────────
    def ask(self):
        """The two read-only commands, in order: who this Mac is, then what
        it serves. Nothing here publishes anything. False when another read
        is already in flight."""
        with self.lock:
            if self.pending:
                return False
            if not self.find_binary():
                self.settle()
                return True
            self.pending, self.cli_err = True, None
        try:
            code, out = self._run(F.status_command())
            info = F.parse_status(out)
            if code != 0 or not info:
                with self.lock:
                    self.status_info = None
                    self.cli_err = F.fatal_reason(out) or F.NO_STATUS
                return True
            with self.lock:
                self.status_info = info
            _, serve_out = self._run(F.serve_command())
            why = F.fatal_reason(serve_out)
            with self.lock:
                if why:
                    self.cli_err = why
                else:
                    # An empty answer, whatever the exit code, is "off".
                    self.serve_info = F.parse_serve(serve_out)
            return True
        finally:
            with self.lock:
                self.pending = False
                self.settle()

    def ask_soon(self, wait=ASK_WAIT_S):
        """ask() in the background, waiting up to `wait` seconds for it."""
        t = threading.Thread(target=self.ask, daemon=True)
        t.start()
        t.join(wait)

    # ── Arming it: the only two calls that change anything ───────────────
    def _switch(self, args, refused, done_line):
        code, out = self._run(args)
        with self.lock:
            self.pending = False
            if code != 0:
                self.cli_err = F.fatal_reason(out) or (refused + first_line(out))
                self.settle()
                return 502
            self.cli_err = None
        log(done_line)
        self.ask()  # read back what tailscaled now holds rather than assume it
        return 200

    def turn_on(self):
        """-> HTTP code. Arms the funnel for the leased port. Never called
        by anything but POST /arm."""
        with self.lock:
            req, _ = F.primary(self.leases, self.now())
            port = (req["port"] if req and req["mode"] == "funnel" else None) or self.port
            if not port:
                self.cli_err = F.NO_LEASE_PORT
                self.settle()
                return 409
            if not self.find_binary():
                self.cli_err = F.NOT_INSTALLED
                self.settle()
                return 503
            if self.pending:
                return 409
            self.pending = True
        return self._switch(F.on_command(port), "tailscale funnel refused: ", "[funnel] armed for port %d" % port)

    def turn_off(self):
        with self.lock:
            if not self.find_binary():
                self.cli_err = F.NOT_INSTALLED
                self.settle()
                return 503
            if self.pending:
                return 409
            self.pending = True
        return self._switch(F.off_command(), "tailscale funnel refused: ", "[funnel] turned off by hand")

    # ── The watchdog ─────────────────────────────────────────────────────
    def watch(self):
        """One tick: drop dead leases, re-read tailscale, probe the origin.
        With no lease, only every tenth tick reads tailscale."""
        with self.lock:
            self.ticks += 1
            for owner in F.expire(self.leases, self.now()):
                log("[funnel] lease expired: %s", owner)
            req, _ = F.primary(self.leases, self.now())
            if (req and req["mode"] == "off") or (not req and self.ticks % F.IDLE_EVERY != 0):
                self.settle()
                return
        self.ask()
        self.probe()

    def probe(self):
        """Does the funnel still reach the origin its owner meant? Three
        misses in a row turn the status to error; an answer clears it."""
        with self.lock:
            req, _ = F.primary(self.leases, self.now())
            if not (self.funnel and self.url and req and req.get("probe_path")):
                return
            url, gen = self.url + req["probe_path"], self.generation
        code, body = self.fetch(url)
        with self.lock:
            if gen != self.generation:
                return  # a different funnel answered
            if code == 200 and F.probe_ok(body, req.get("probe_expect")):
                if self.misses > 0:
                    log("[funnel] %s answers again", self.url)
                self.misses = 0
            else:
                self.misses += 1
                log("[funnel] probe miss %d (http %s)", self.misses, code)
            self.settle()

    # ── What the endpoints call ──────────────────────────────────────────
    def lease(self, req):
        """-> (snapshot, HTTP code)."""
        with self.lock:
            now = self.now()
            F.expire(self.leases, now)
            had = self.leases.get(req["owner"])
            # One funnel serves one origin. The owner naming the port is exempt.
            _, first = F.primary(self.leases, now)
            if first and first != req["owner"]:
                clash = F.conflict({"port": self.port, "mode": self.mode}, req)
                if clash:
                    log("[funnel] refused %s: %s", req["owner"], clash)
                    snap = self.state()
                    snap["error"] = clash
                    return snap, 409
            self.seq += 1
            self.leases[req["owner"]] = {
                "expires": F.lease_expiry(req, now),
                "taken": had["taken"] if had else self.seq,  # renewing keeps your place
                "req": req,
            }
            fresh = not had or F.req_key(had["req"]) != F.req_key(req)
            if not had:
                log("[funnel] lease taken by %s (%s:%s)", req["owner"], req["mode"], req["port"])
            self.settle()
        # A renewal that asked for the same thing is answered from what the
        # watchdog already knows; a new or changed lease reads tailscale.
        if fresh and req["mode"] == "funnel":
            self.ask_soon()
        with self.lock:
            return self.state(), 503 if self.fatal else 200

    def release(self, owner):
        with self.lock:
            if self.leases.pop(owner, None) is not None:
                log("[funnel] lease released by %s", owner)
            self.settle()
            return self.state()

    def view(self):
        """The state plus everything the tile draws."""
        with self.lock:
            was = self.bin
            self.find_binary()
            if was != self.bin:
                self.settle()
            now, raw = self.now(), self.raw()
            snap = F.snapshot(raw, now)
            steps = F.setup_steps(
                {"bin": self.bin, "status": self.status_info, "hostname": self.hostname,
                 "port": self.port, "funnel": self.funnel, "url": self.url}
            )
            nxt = F.next_step(steps)
            return {
                **snap,
                "owner": self.owner,
                "pending": self.pending,
                "installed": self.bin is not None,
                "auth_url": (self.status_info or {}).get("auth_url"),
                "status_line": F.status_line(raw, now),
                "url_line": F.url_line(raw),
                "copyable": F.copyable(raw),
                "lease_line": F.lease_line(snap["leases"]),
                "setup_line": F.setup_line(steps),
                "next_step": dict(nxt, button=F.ACTION_LABEL.get(nxt.get("action"))) if nxt else None,
                "steps": [dict(s, line=F.step_line(s)) for s in steps],
                "webhook_url": F.webhook_url(self.hostname),
                "env_lines": F.env_lines(self.hostname),
                "policy_snippet": F.POLICY_SNIPPET,
            }


def json_error(message):
    return json.dumps({"error": message}) + "\n"


def route(funnel, method, path, body):
    """-> (HTTP code, JSON text). Pure apart from what `funnel` does."""
    target = urlsplit(path or "/").path
    query = F.params(path)
    if target == "/health":
        return 200, json.dumps({"ready": True})
    if target == "/lease":
        req = F.request(body, query)
        bad = F.bad_request(req)
        if bad:
            return 400, json_error(bad)
        snap, code = funnel.lease(req)
        return code, F.encode_state(snap)
    if target in ("/funnel", "/tunnel"):
        return 200, F.encode_state(funnel.state())
    if target == "/release":
        req = F.request(body, query)
        if not isinstance(req["owner"], str) or not req["owner"]:
            return 400, json_error("owner is required")
        return 200, F.encode_state(funnel.release(req["owner"]))
    if target == "/view":
        return 200, json.dumps(funnel.view(), ensure_ascii=False) + "\n"
    if target == "/check":
        if not funnel.ask():
            return 409, json_error("tailscale is already being asked")
        return 200, F.encode_state(funnel.state())
    if target in ("/arm", "/disarm"):
        if method != "POST":
            return 405, json_error("%s changes the funnel: POST only" % target)
        code = funnel.turn_on() if target == "/arm" else funnel.turn_off()
        return code, F.encode_state(funnel.state())
    return 404, json_error("no such endpoint: " + target)


class Handler(BaseHTTPRequestHandler):
    funnel = None  # set by main() or the tests

    def do_GET(self):
        self.handle_any()

    def do_POST(self):
        self.handle_any()

    def handle_any(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            self.reply(413, json_error("the body is larger than 64 KiB"))
            return
        body = self.rfile.read(length).decode("utf-8", "replace") if length else ""
        try:
            code, text = route(self.funnel, self.command, self.path, body)
        except Exception as e:  # the reason goes back to the caller
            log("[funnel] request failed: %r", e)
            code, text = 500, json_error("%s: %s" % (type(e).__name__, e))
        self.reply(code, text)

    def reply(self, status, body, content_type="application/json"):
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, format, *args):
        # The watchdog's lines matter more than every 30 s /lease renewal.
        if self.command == "POST" or "/health" not in self.path and "/view" not in self.path:
            sys.stderr.write(format % args + "\n")


def watchdog(funnel, stop, period=F.WATCH_S):
    """Reads tailscale once at start, so the tile has a hostname, then ticks."""
    try:
        funnel.ask()
    except Exception as e:
        log("[funnel] first read failed: %r", e)
    while not stop.wait(period):
        try:
            funnel.watch()
        except Exception as e:  # the watchdog must outlive one bad round
            log("[funnel] watchdog round failed: %r", e)


def main():
    funnel = Funnel()
    Handler.funnel = funnel
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    stop = threading.Event()
    threading.Thread(target=watchdog, args=(funnel, stop), daemon=True).start()
    log("tailscale-funnel listening on http://%s:%d, tailscale at %s", HOST, PORT, funnel.find_binary())
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        stop.set()


if __name__ == "__main__":
    main()
