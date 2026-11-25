"""Serves crates/ui for the hub preview, with caching off, so an edit to
crates/ui/ui shows on the next load. Standard library only.

    python3 crates/ui/dev/serve.py [port]    # default 8765

Then open http://localhost:<port>/dev/hub.html.
"""

import functools
import http.server
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


class NoStore(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    handler = functools.partial(NoStore, directory=str(ROOT))
    with http.server.ThreadingHTTPServer(("", port), handler) as server:
        print(f"Serving {ROOT} on http://localhost:{port}/dev/hub.html", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
