#!/usr/bin/env python3
"""__NAME__: a sidecar for bricolo.

A small HTTP server on 127.0.0.1 that the bricolo node starts, health-checks
and restarts (`bricolo sidecar add`). Standard library only.

    GET  /health   {"ready": true} once warm_up() has finished; until then
                   {"ready": false}, and the node shows "starting".
    POST /process  the request body as UTF-8 text in, text out. The
                   example upper-cases it. Errors answer 4xx or 5xx with
                   the reason as the body.

Put your work in warm_up() (load a model once) and process(). Run it by
hand with `python3 server.py`; the port is __PORT__, or $SIDECAR_PORT.
"""

import json
import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
PORT = int(os.environ.get("SIDECAR_PORT", "__PORT__"))
MAX_BODY = 8 * 1024 * 1024

ready = threading.Event()


def warm_up():
    """Loads what process() needs, once. Runs in the background, so
    /health answers {"ready": false} meanwhile."""
    ready.set()


def process(text):
    """The work: text in, text out."""
    return text.upper()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.reply(200, json.dumps({"ready": ready.is_set()}), "application/json")
        else:
            self.reply(404, "no such endpoint\n")

    def do_POST(self):
        if self.path != "/process":
            self.reply(404, "no such endpoint\n")
            return
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            self.reply(413, "the body is larger than 8 MiB\n")
            return
        if not ready.is_set():
            self.reply(503, "still warming up\n")
            return
        text = self.rfile.read(length).decode("utf-8", "replace")
        try:
            answer = process(text)
        except Exception as e:  # the reason goes back to the caller
            self.reply(500, f"{type(e).__name__}: {e}\n")
            return
        self.reply(200, answer)

    def reply(self, status, body, content_type="text/plain; charset=utf-8"):
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, format, *args):
        # stderr goes to <data>/logs/sidecar-__NAME__.log under the node.
        sys.stderr.write("%s %s\n" % (self.command, format % args))


def main():
    server = ThreadingHTTPServer((HOST, PORT), Handler)
    threading.Thread(target=warm_up, daemon=True).start()
    print(f"__NAME__ listening on http://{HOST}:{PORT}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
