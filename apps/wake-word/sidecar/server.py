#!/usr/bin/env python3
"""wake-word: say "Codec" with no call open and the voice agent starts one.
A sidecar for bricolo.

A port of apps/wake_word and lib/wake_word from the Hammerspoon setup. There
is no wake-word model: an ffmpeg child tails the microphone into a raw file,
and every 1.5 s the last 4 s, when loud enough to be speech, go to the warm
parakeet sidecar (:8765 /transcribe). A transcript with the word in it starts
a call on the voice agent's daemon (:8792 POST /start), then the speech in
front of the word is transcribed again and handed over with POST /heard, so
the call opens on the sentence the word was part of.

The microphone is open only while enabled. The wake-word plugin (apps/wake-word/plugin)
owns the switch and tells this server; the server starts disabled.

    GET  /health    {"ready": true}
    GET  /status    {"enabled", "capturing", "state", "why", "device", "last_heard"}
    POST /enable    body: the microphone's avfoundation name or index; an
                    empty body takes [mic] device from node.toml
    POST /disable   closes the microphone

Standard library only. $SIDECAR_PORT (8803), $STT_URL, $AGENT_URL, $FFMPEG
and $BRICOLO_NODE_TOML override the defaults.
"""

import json
import math
import os
import struct
import subprocess
import sys
import threading
import time
import tomllib
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
PORT = int(os.environ.get("SIDECAR_PORT", "8803"))
STT = os.environ.get("STT_URL", "http://127.0.0.1:8765")
AGENT = os.environ.get("AGENT_URL", "http://127.0.0.1:8792")
FFMPEG = os.environ.get("FFMPEG", "/opt/homebrew/bin/ffmpeg")
NODE_TOML = os.path.expanduser(os.environ.get("BRICOLO_NODE_TOML", "~/.config/bricolo/node.toml"))

WORD = "Codec"
# What Parakeet writes when it hears "Codec".
VARIANTS = ("codec", "kodec", "kodak", "kodek", "codeck", "codack")
RATE = 16000
BYTES_PER_S = RATE * 2
WINDOW_S = 4
TICK_S = 1.5
# A silent room measures in the tens, speech in the hundreds to thousands.
RMS_FLOOR = 180
# Less than this is a window that only just started filling.
MIN_S = 0.8
ROTATE_BYTES = BYTES_PER_S * 60 * 15
# One trigger, then a pause: the windows overlap, so the word shows up twice.
COOLDOWN_S = 8
# After a call, wait until the window holds only audio from after it.
SETTLE_S = WINDOW_S + TICK_S
PREROLL_MAX_S = 10
RAW = "/tmp/bricolo-wake.raw"
WAV = "/tmp/bricolo-wake-window.wav"
PREROLL_WAV = "/tmp/bricolo-wake-preroll.wav"


# ── Pure rules, from lib/wake_word ───────────────────────────────────────────

def match(text):
    if not isinstance(text, str):
        return False
    norm = "".join(c if c.isalnum() else " " for c in text.lower())
    return any(v in norm for v in VARIANTS)


def window(size, window_s=WINDOW_S, min_s=MIN_S):
    """(offset, length) of the last window_s seconds, or None if too short."""
    if size < int(min_s * BYTES_PER_S):
        return None
    length = min(size, int(window_s * BYTES_PER_S))
    length -= length % 2
    offset = size - length
    offset -= offset % 2
    return offset, length


def preroll(size, start, max_s=PREROLL_MAX_S):
    """(offset, length) from `start` to the end, at most max_s seconds."""
    start = max(0, start)
    if start >= size:
        return None
    length = min(size - start, int(max_s * BYTES_PER_S))
    length -= length % 2
    start -= start % 2
    if length < 2:
        return None
    return start, length


def utterance(text):
    """The transcript, or None when it holds nothing but the word."""
    if not isinstance(text, str):
        return None
    clean = " ".join(text.split())
    if not clean:
        return None
    rest = "".join(c if c.isalnum() else " " for c in clean.lower())
    for v in sorted(VARIANTS, key=len, reverse=True):
        rest = rest.replace(v, " ")
    return clean if rest.strip() else None


def rms(pcm, stride=8):
    frames = len(pcm) // 2
    if frames == 0:
        return 0.0
    samples = struct.unpack("<%dh" % frames, pcm[: frames * 2])[::stride]
    return math.sqrt(sum(s * s for s in samples) / len(samples))


def wav_header(data_len, rate=RATE, channels=1):
    bits = 16
    block = channels * bits // 8
    return (b"RIFF" + struct.pack("<I", 36 + data_len) + b"WAVEfmt "
            + struct.pack("<IHHIIHH", 16, 1, channels, rate, rate * block, block, bits)
            + b"data" + struct.pack("<I", data_len))


def daemon_state(code, body):
    """The voice agent's /status as idle, live, agent_down or down."""
    try:
        doc = json.loads(body) if code == 200 else None
    except (TypeError, ValueError):
        doc = None
    if not isinstance(doc, dict) or not isinstance(doc.get("running"), bool):
        return "down"
    if doc["running"]:
        return "live"
    return "agent_down" if doc.get("agent") == "down" else "idle"


def listening(enabled, state, settled):
    return bool(enabled) and settled and state == "idle"


def fire(now, last, cooldown=COOLDOWN_S):
    return last is None or now - last >= cooldown


def settled(now, ended_at, settle=SETTLE_S):
    return ended_at is None or now - ended_at >= settle


def why(enabled, state, capturing):
    if not enabled:
        return "off"
    if state == "live":
        return "paused, call in progress"
    if state != "idle":
        return "waiting for the voice agent's daemon"
    if not capturing:
        return "starting the microphone"
    return "listening"


def mic_device(path=NODE_TOML):
    """[mic] device from node.toml, the microphone bricolo records from, or
    "default"."""
    try:
        with open(path, "rb") as f:
            return tomllib.load(f).get("mic", {}).get("device") or "default"
    except (OSError, tomllib.TOMLDecodeError):
        return "default"


def capture_args(device, raw=RAW):
    return [FFMPEG, "-y", "-hide_banner", "-loglevel", "error",
            "-f", "avfoundation", "-i", ":" + str(device),
            "-ar", str(RATE), "-ac", "1", "-f", "s16le", "-flush_packets", "1", raw]


# ── The listener ────────────────────────────────────────────────────────────

def http(method, url, body=b"", headers=None, timeout=10):
    """(status, body text); status 0 when nothing answered."""
    req = urllib.request.Request(url, data=body if method == "POST" else None,
                                 headers=headers or {}, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except (urllib.error.URLError, OSError):
        return 0, ""


def log(msg):
    sys.stderr.write(msg + "\n")
    sys.stderr.flush()


class Listener:
    """The capture child, the tick, and the calls. `spawn`, `http` and
    `clock` are swapped out by the tests."""

    def __init__(self, spawn=subprocess.Popen, call=http, clock=time.time):
        self.spawn, self.call, self.clock = spawn, call, clock
        self.lock = threading.Lock()
        self.enabled = False
        self.device = "default"
        self.child = None
        self.state = "down"
        self.last_state = None
        self.call_ended_at = None
        self.last_fired = None
        self.last_heard = None

    # The microphone.
    def start_capture(self):
        self.stop_capture()
        try:
            os.remove(RAW)
        except FileNotFoundError:
            pass
        self.child = self.spawn(capture_args(self.device), stdin=subprocess.DEVNULL)
        log(f"listening on {self.device!r} for {WORD!r}")

    def stop_capture(self):
        if self.child is not None:
            self.child.terminate()
            try:
                self.child.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.child.kill()
            self.child = None

    def capturing(self):
        return self.child is not None and self.child.poll() is None

    def enable(self, device):
        with self.lock:
            self.enabled, self.device = True, device or "default"
            self.start_capture()

    def disable(self):
        with self.lock:
            self.enabled = False
            self.stop_capture()

    def status(self):
        return {"enabled": self.enabled, "capturing": self.capturing(), "state": self.state,
                "why": why(self.enabled, self.state, self.capturing()), "device": self.device,
                "last_heard": self.last_heard}

    # One tick.
    def tick(self):
        if not self.enabled:
            return
        now = self.clock()
        code, body = self.call("GET", AGENT + "/status")
        self.state = daemon_state(code, body)
        if self.last_state == "live" and self.state != "live":
            self.call_ended_at = now
        self.last_state = self.state
        if not self.capturing():
            self.start_capture()
            return
        try:
            size = os.path.getsize(RAW)
        except OSError:
            return
        if size >= ROTATE_BYTES:
            log(f"capture file reached {size / 1048576:.0f} MB; starting a fresh one")
            self.start_capture()
            return
        if not listening(True, self.state, settled(now, self.call_ended_at)):
            return
        span = window(size)
        if not span:
            return
        pcm = read_span(RAW, *span)
        if len(pcm) < 2 or rms(pcm) < RMS_FLOOR:
            return
        write_wav(WAV, pcm)
        code, text = self.call("POST", STT + "/transcribe", WAV.encode())
        if code == 200 and match(text):
            self.wake(text, span[0])

    def wake(self, heard, start):
        now = self.clock()
        if not fire(now, self.last_fired):
            return
        self.last_fired, self.last_heard = now, heard.strip()
        log(f"heard {WORD!r} in {heard.strip()!r}")
        code, body = self.call("POST", AGENT + "/start", b"{}", {"Content-Type": "application/json"})
        self.state = daemon_state(code, body)
        if code != 200:
            log(f"daemon refused /start: {code} {body[:120]}")
            return
        self.send_preroll(start, heard)

    def send_preroll(self, start, fallback):
        text = None
        try:
            span = preroll(os.path.getsize(RAW), start)
        except OSError:
            span = None
        if span:
            write_wav(PREROLL_WAV, read_span(RAW, *span))
            code, body = self.call("POST", STT + "/transcribe", PREROLL_WAV.encode())
            if code == 200 and match(body):
                text = utterance(body)
                if text is None:
                    log(f"nothing said before {WORD!r}; the call opens listening")
                    return
        if text is None:
            text = utterance(fallback)
        if text:
            code, body = self.call("POST", AGENT + "/heard", json.dumps({"text": text}).encode(),
                                   {"Content-Type": "application/json"})
            log(f"preroll {text!r}" if code == 200 else f"daemon refused the preroll: {code}")

    def run(self, stop):
        while not stop.wait(TICK_S):
            try:
                self.tick()
            except Exception as e:  # one bad tick must not end the listener
                log(f"tick failed: {type(e).__name__}: {e}")


def read_span(path, offset, length):
    with open(path, "rb") as f:
        f.seek(offset)
        return f.read(length)


def write_wav(path, pcm):
    with open(path, "wb") as f:
        f.write(wav_header(len(pcm)))
        f.write(pcm)


listener = Listener()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.reply(200, json.dumps({"ready": True}), "application/json")
        elif self.path == "/status":
            self.reply(200, json.dumps(listener.status()), "application/json")
        else:
            self.reply(404, "no such endpoint\n")

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(min(length, 1024)).decode("utf-8", "replace").strip()
        if self.path == "/enable":
            listener.enable(body or mic_device())
        elif self.path == "/disable":
            listener.disable()
        else:
            self.reply(404, "no such endpoint\n")
            return
        self.reply(200, json.dumps(listener.status()), "application/json")

    def reply(self, status, body, content_type="text/plain; charset=utf-8"):
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, format, *args):
        # stderr goes to <data>/logs/sidecar-wake-word.log under the node.
        sys.stderr.write("%s %s\n" % (self.command, format % args))


def main():
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    stop = threading.Event()
    threading.Thread(target=listener.run, args=(stop,), daemon=True).start()
    print(f"wake-word listening on http://{HOST}:{PORT}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        stop.set()
        listener.disable()


if __name__ == "__main__":
    main()
