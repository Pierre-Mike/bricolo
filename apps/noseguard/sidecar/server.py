#!/usr/bin/env python3
"""noseguard: the nose-touch watcher's daemon keeper, a sidecar for
bricolo.

Runs the camera daemon in daemon/ (noseguard.py) as a child process through
run_daemon.py, under `uv run --script` (or the python in $NOSEGUARD_PYTHON).
run_daemon.py sends the daemon's touch, ready and error events down a pipe
instead of opening hammerspoon:// URLs. On a touch it does what the Hammerspoon plugin did: a
red flash through daemon/overlay/overlay (hidden from screen sharing) and the
Sosumi alarm twice, and it counts. The noseguard script
(apps/noseguard/plugin) owns the widget and the settings. Standard library
only; the camera opens only between /start and /stop.

    GET  /health   {"ready": true}
    GET  /status   {"watching", "count", "camera_id", "camera_name",
                   "sensitivity", "hold", "debug", "last_event",
                   "last_event_at", "last_error"}
    POST /start    JSON {"camera_id", "camera_name", "sensitivity" 0-100,
                   "hold" seconds, "debug"}, every key optional: starts the
                   daemon, or restarts it with the new settings
    POST /stop     stops the daemon; the camera light goes out
    GET  /cameras  the daemon's `noseguard.py list`: [{"id", "name",
                   "builtin", "connected", "index"}]
    POST /reset    sets the touch count to 0
    POST /flash    one flash and alarm, like a touch, without counting it

Errors answer 4xx or 5xx with the reason as the body. The port is 8802, or
$SIDECAR_PORT; $NOSEGUARD_DIR moves the daemon's folder and
$NOSEGUARD_PYTHON a python to use instead of uv, and $NOSEGUARD_UV the uv to
use (default: uv on PATH, else ~/.local/bin/uv).
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
PORT = int(os.environ.get("SIDECAR_PORT", "8802"))
NOSEGUARD_DIR = os.path.expanduser(
    os.environ.get("NOSEGUARD_DIR", os.path.join(os.path.dirname(os.path.abspath(__file__)), "daemon")))
RUNNER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "run_daemon.py")


def daemon_command(env):
    """How to run run_daemon.py: under the python in $NOSEGUARD_PYTHON, or
    under uv, which builds and caches the venv its script block names. The
    node starts sidecars with a short PATH, so uv falls back to where its
    installer puts it."""
    if env.get("NOSEGUARD_PYTHON"):
        return [env["NOSEGUARD_PYTHON"], RUNNER]
    uv = env.get("NOSEGUARD_UV") or shutil.which("uv", path=env.get("PATH")) \
        or os.path.expanduser("~/.local/bin/uv")
    return [uv, "run", "--script", RUNNER]


DAEMON = daemon_command(os.environ)
OVERLAY = os.path.join(NOSEGUARD_DIR, "overlay/overlay")
AFPLAY = "/usr/bin/afplay"
# hs.sound.getByName("Sosumi") at its default volume, twice, 0.35 s apart.
ALARM = "/System/Library/Sounds/Sosumi.aiff"
ALARM_GAP_S = 0.35
FLASH_S = "1.2"
EVENTS = ("touch", "ready", "error")
LIST_TIMEOUT_S = 20
MAX_BODY = 4096

DEFAULTS = {"camera_id": "", "camera_name": "", "sensitivity": 55, "hold": 0.5, "debug": False}


def parse_settings(body):
    """The /start body -> a full settings dict. Raises ValueError with the
    reason. Missing keys keep their defaults."""
    try:
        doc = json.loads(body) if body.strip() else {}
    except json.JSONDecodeError as e:
        raise ValueError(f"the body is not JSON: {e}")
    if not isinstance(doc, dict):
        raise ValueError("the body is a JSON object")
    unknown = set(doc) - set(DEFAULTS)
    if unknown:
        raise ValueError(f"unknown settings: {', '.join(sorted(unknown))}")
    s = dict(DEFAULTS, **doc)
    for key in ("camera_id", "camera_name"):
        if not isinstance(s[key], str):
            raise ValueError(f"{key} is a string")
    sens, hold = s["sensitivity"], s["hold"]
    if isinstance(sens, bool) or not isinstance(sens, (int, float)) or not 0 <= sens <= 100:
        raise ValueError("sensitivity is a number from 0 to 100")
    if isinstance(hold, bool) or not isinstance(hold, (int, float)) or not 0 < hold <= 10:
        raise ValueError("hold is a number of seconds above 0, at most 10")
    if not isinstance(s["debug"], bool):
        raise ValueError("debug is true or false")
    s["camera_id"] = s["camera_id"].strip()
    if not s["camera_id"]:
        s["camera_name"] = ""
    return s


def child_env(settings, base):
    """The daemon's environment: the NG_* knobs init.lua set, on top of
    `base`. NG_EVENT_FD is added by Guard.start."""
    env = dict(base)
    env.update({
        "NG_SENS": f"{settings['sensitivity']:g}",
        "NG_HOLD": f"{settings['hold']:g}",
        "NG_CAM_ID": settings["camera_id"],
        "NG_DEBUG": "1" if settings["debug"] else "0",
        "NOSEGUARD_DIR": NOSEGUARD_DIR,
        "PYTHONUNBUFFERED": "1",
    })
    return env


def parse_event(line):
    """One line from the event pipe -> 'touch', 'ready' or 'error', else None."""
    event = line.decode("utf-8", "replace").strip() if isinstance(line, bytes) else line.strip()
    return event if event in EVENTS else None


def effects(overlay_ok):
    """What a touch sets off, as (delay in seconds, argv): the flash when the
    overlay binary exists, and the alarm twice."""
    out = [(0.0, [OVERLAY, FLASH_S])] if overlay_ok else []
    return out + [(0.0, [AFPLAY, ALARM]), (ALARM_GAP_S, [AFPLAY, ALARM])]


def parse_cameras(stdout):
    """`noseguard.py list` output -> the list. Raises ValueError."""
    try:
        cams = json.loads(stdout)
    except json.JSONDecodeError as e:
        raise ValueError(f"noseguard.py list printed no JSON: {e}")
    if not isinstance(cams, list):
        raise ValueError("noseguard.py list printed no list")
    return cams


def now_iso():
    return time.strftime("%Y-%m-%dT%H:%M:%S%z")


class Guard:
    """One daemon at a time, the touch count and the last event. `spawn`,
    `run` and `later` are swapped out by the tests, so they start no
    process, open no camera and play nothing."""

    def __init__(self, spawn=subprocess.Popen, run=subprocess.run, later=None,
                 overlay_ok=None):
        self.spawn = spawn
        self.run = run
        self.later = later or self._later
        self.overlay_ok = overlay_ok if overlay_ok is not None else os.access(OVERLAY, os.X_OK)
        self.lock = threading.Lock()
        self.child = None
        self.settings = dict(DEFAULTS)
        self.count = 0
        self.last_event = None
        self.last_event_at = None
        self.last_error = None

    @staticmethod
    def _later(delay, fn):
        if delay <= 0:
            fn()
        else:
            t = threading.Timer(delay, fn)
            t.daemon = True
            t.start()

    def watching(self):
        return self.child is not None and self.child.poll() is None

    def start(self, settings):
        with self.lock:
            self._stop()
            self.settings = settings
            self.last_error = None
            r, w = os.pipe()
            env = child_env(settings, os.environ)
            env["NG_EVENT_FD"] = str(w)
            try:
                child = self.spawn(DAEMON, env=env, cwd=NOSEGUARD_DIR,
                                   stdin=subprocess.DEVNULL, pass_fds=(w,))
            except OSError as e:
                os.close(r)
                self.last_error = f"cannot start {DAEMON[0]}: {e}"
                raise
            finally:
                os.close(w)
            self.child = child
            print(f"noseguard: daemon started camera={settings['camera_id'] or 'auto'} "
                  f"sens={settings['sensitivity']:g} hold={settings['hold']:g} "
                  f"debug={settings['debug']}", flush=True)
        threading.Thread(target=self._read_events, args=(r, child), daemon=True).start()

    def stop(self):
        with self.lock:
            self._stop()

    def _stop(self):
        if self.child is None:
            return
        child, self.child = self.child, None
        child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()
        print("noseguard: daemon stopped", flush=True)

    def _read_events(self, fd, child):
        """Reads the daemon's event pipe until it closes, which is when the
        daemon exits."""
        with os.fdopen(fd, "rb") as pipe:
            for line in pipe:
                event = parse_event(line)
                if event:
                    self.on_event(event, child)
        code = child.wait()
        with self.lock:
            if self.child is not child:
                return  # stopped or replaced on purpose
            self.child = None
            if self.last_error is None:
                self.last_error = f"the daemon exited (code {code}); see bricolo sidecar logs noseguard"
        print(f"noseguard: daemon exited code={code}", flush=True)

    def on_event(self, event, child):
        with self.lock:
            if self.child is not child:
                return
            self.last_event, self.last_event_at = event, now_iso()
            if event == "touch":
                self.count += 1
            elif event == "error":
                self.last_error = "camera unavailable; see bricolo sidecar logs noseguard"
        print(f"noseguard: event {event}", flush=True)
        if event == "touch":
            self.alert()

    def alert(self):
        for delay, argv in effects(self.overlay_ok):
            self.later(delay, lambda argv=argv: self._spawn_quietly(argv))

    def _spawn_quietly(self, argv):
        try:
            self.spawn(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL)
        except OSError as e:
            print(f"noseguard: cannot run {argv[0]}: {e}", file=sys.stderr, flush=True)

    def reset(self):
        with self.lock:
            self.count = 0

    def cameras(self):
        r = self.run(DAEMON + ["list"],
                     cwd=NOSEGUARD_DIR, capture_output=True, text=True,
                     timeout=LIST_TIMEOUT_S, stdin=subprocess.DEVNULL)
        if r.returncode != 0:
            raise ValueError(f"run_daemon.py list failed: {r.stderr.strip()[-300:]}")
        return parse_cameras(r.stdout)

    def status(self):
        with self.lock:
            return dict(self.settings, watching=self.watching(), count=self.count,
                        last_event=self.last_event, last_event_at=self.last_event_at,
                        last_error=self.last_error)


guard = Guard()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.reply_json(200, {"ready": True})
        elif self.path == "/status":
            self.reply_json(200, guard.status())
        elif self.path == "/cameras":
            try:
                self.reply_json(200, guard.cameras())
            except (OSError, ValueError, subprocess.TimeoutExpired) as e:
                self.reply(502, f"{e}\n")
        else:
            self.reply(404, "no such endpoint\n")

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            self.reply(413, "the body is too large\n")
            return
        body = self.rfile.read(length).decode("utf-8", "replace")
        if self.path == "/start":
            try:
                guard.start(parse_settings(body))
            except ValueError as e:
                self.reply(400, f"{e}\n")
                return
            except OSError as e:
                self.reply(500, f"{e}\n")
                return
        elif self.path == "/stop":
            guard.stop()
        elif self.path == "/reset":
            guard.reset()
        elif self.path == "/flash":
            guard.alert()
        else:
            self.reply(404, "no such endpoint\n")
            return
        self.reply_json(200, guard.status())

    def reply_json(self, status, doc):
        self.reply(status, json.dumps(doc), "application/json")

    def reply(self, status, body, content_type="text/plain; charset=utf-8"):
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, format, *args):
        # stderr goes to <data>/logs/sidecar-noseguard.log under the node.
        sys.stderr.write("%s %s\n" % (self.command, format % args))


def main():
    # The node stops a sidecar with SIGTERM; stop the daemon on the way out
    # so the camera light never outlives the server.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"noseguard listening on http://{HOST}:{PORT}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        guard.stop()


if __name__ == "__main__":
    main()
