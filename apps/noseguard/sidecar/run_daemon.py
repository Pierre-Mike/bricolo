#!/usr/bin/env python3
"""Runs the noseguard camera daemon for the noseguard sidecar.

server.py starts this with the daemon's venv python and the NG_* settings in
the environment. It imports noseguard.py unchanged from $NOSEGUARD_DIR and
replaces its fire(), which ran `open -g hammerspoon://noseguard?event=...`,
with one that writes the event as a line to the pipe in $NG_EVENT_FD. The
daemon's own output goes to stdout and stderr, so it lands in the sidecar's
log. When the sidecar goes away, this exits too, so an orphan never keeps
the camera light on.
"""

import os
import sys
import threading
import time

NOSEGUARD_DIR = os.path.expanduser(
    os.environ.get("NOSEGUARD_DIR", os.path.join(os.path.dirname(os.path.abspath(__file__)), "daemon")))


def event_line(event):
    return f"{event}\n".encode("utf-8")


def make_fire(fd, exit=os._exit):
    """The replacement for noseguard.fire: one small write to the pipe, which
    a pipe keeps whole. A closed pipe means the sidecar is gone."""
    def fire(event):
        try:
            os.write(fd, event_line(event))
        except OSError:
            exit(0)
    return fire


def watch_parent(ppid, getppid=os.getppid, sleep=time.sleep, exit=os._exit):
    """Exits once the parent changes: the sidecar died and launchd adopted us."""
    while getppid() == ppid:
        sleep(1)
    exit(0)


def install(module, fd):
    if not callable(getattr(module, "fire", None)):
        raise SystemExit("noseguard.py has no fire(); run_daemon.py needs updating")
    module.fire = make_fire(fd)


def main():
    fd = int(os.environ["NG_EVENT_FD"])
    sys.path.insert(0, NOSEGUARD_DIR)
    import noseguard

    install(noseguard, fd)
    threading.Thread(target=watch_parent, args=(os.getppid(),), daemon=True).start()
    noseguard.main()


if __name__ == "__main__":
    main()
