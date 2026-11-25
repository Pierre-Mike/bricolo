"""Tests for server.py and run_daemon.py: `python3 -m unittest` in this
folder. Nothing real starts: the guard spawns fake children instead of the
daemon, the overlay and afplay, and lists cameras through a fake run."""

import json
import os
import subprocess
import threading
import time
import types
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

import run_daemon
import server


class FakeChild:
    """A spawned process. A daemon child keeps a copy of the event pipe's
    write end, so a test can send events and then close it, as an exit."""

    def __init__(self, argv, **kwargs):
        self.argv = argv
        self.kwargs = kwargs
        self.code = None
        fds = kwargs.get("pass_fds") or ()
        self.events = os.dup(fds[0]) if fds else None

    def send(self, event):
        os.write(self.events, run_daemon.event_line(event))

    def exit(self, code):
        self.code = code
        os.close(self.events)

    def poll(self):
        return self.code

    def terminate(self):
        if self.code is None:
            self.exit(-15)

    def kill(self):
        self.terminate()

    def wait(self, timeout=None):
        return self.code


class Spawner:
    def __init__(self):
        self.children = []

    def __call__(self, argv, **kwargs):
        child = FakeChild(argv, **kwargs)
        self.children.append(child)
        return child

    def daemons(self):
        return [c for c in self.children if c.argv[-1] == server.RUNNER]

    def effects(self):
        return [c.argv for c in self.children if c.argv[-1] != server.RUNNER]


def eventually(check, timeout=2.0):
    end = time.time() + timeout
    while time.time() < end:
        if check():
            return True
        time.sleep(0.01)
    return check()


def now(delay, fn):
    fn()


class PureTest(unittest.TestCase):
    def test_parse_settings_defaults(self):
        self.assertEqual(server.parse_settings(""), server.DEFAULTS)

    def test_parse_settings(self):
        s = server.parse_settings(json.dumps({
            "camera_id": " 0x1430 ", "camera_name": "HD Webcam C525",
            "sensitivity": 35, "hold": 1, "debug": True}))
        self.assertEqual(s, {"camera_id": "0x1430", "camera_name": "HD Webcam C525",
                             "sensitivity": 35, "hold": 1, "debug": True})

    def test_parse_settings_auto_camera_has_no_name(self):
        s = server.parse_settings('{"camera_id": "", "camera_name": "Old"}')
        self.assertEqual(s["camera_name"], "")

    def test_parse_settings_rejects(self):
        for body in ["[1]", "{", '{"sensitivity": 101}', '{"sensitivity": "55"}',
                     '{"hold": 0}', '{"hold": 11}', '{"debug": "yes"}',
                     '{"camera_id": 3}', '{"sens": 55}', '{"sensitivity": true}']:
            with self.assertRaises(ValueError, msg=body):
                server.parse_settings(body)

    def test_child_env(self):
        s = dict(server.DEFAULTS, camera_id="cam", sensitivity=75, hold=0.25, debug=True)
        env = server.child_env(s, {"HOME": "/h"})
        self.assertEqual(env["HOME"], "/h")
        self.assertEqual((env["NG_SENS"], env["NG_HOLD"], env["NG_CAM_ID"], env["NG_DEBUG"]),
                         ("75", "0.25", "cam", "1"))

    def test_parse_event(self):
        self.assertEqual(server.parse_event(b"touch\n"), "touch")
        self.assertEqual(server.parse_event("ready"), "ready")
        self.assertIsNone(server.parse_event(b"noseguard: TOUCH\n"))

    def test_effects(self):
        self.assertEqual(server.effects(True), [
            (0.0, [server.OVERLAY, "1.2"]),
            (0.0, [server.AFPLAY, server.ALARM]),
            (0.35, [server.AFPLAY, server.ALARM]),
        ])
        self.assertNotIn([server.OVERLAY, "1.2"], [argv for _, argv in server.effects(False)])

    def test_parse_cameras(self):
        cams = server.parse_cameras('[{"id": "a", "name": "A", "connected": true}]')
        self.assertEqual(cams[0]["name"], "A")
        for out in ["", "{}", "oops"]:
            with self.assertRaises(ValueError, msg=out):
                server.parse_cameras(out)


class RunDaemonTest(unittest.TestCase):
    def test_install_replaces_fire_with_a_pipe_writer(self):
        r, w = os.pipe()
        module = types.SimpleNamespace(fire=lambda event: self.fail("open ran"))
        run_daemon.install(module, w)
        module.fire("touch")
        module.fire("ready")
        os.close(w)
        with os.fdopen(r, "rb") as pipe:
            self.assertEqual([server.parse_event(line) for line in pipe], ["touch", "ready"])

    def test_install_needs_fire(self):
        with self.assertRaises(SystemExit):
            run_daemon.install(types.SimpleNamespace(), 1)

    def test_closed_pipe_exits(self):
        r, w = os.pipe()
        os.close(r)
        exits = []
        run_daemon.make_fire(w, exit=exits.append)("touch")
        os.close(w)
        self.assertEqual(exits, [0])

    def test_watch_parent_exits_when_adopted(self):
        parents = iter([42, 42, 1])
        exits = []
        run_daemon.watch_parent(42, getppid=lambda: next(parents), sleep=lambda _: None,
                                exit=exits.append)
        self.assertEqual(exits, [0])


class GuardTest(unittest.TestCase):
    def setUp(self):
        self.spawn = Spawner()
        self.guard = server.Guard(spawn=self.spawn, run=None, later=now, overlay_ok=True)

    def tearDown(self):
        self.guard.stop()

    def test_starts_off(self):
        st = self.guard.status()
        self.assertFalse(st["watching"])
        self.assertEqual(st["count"], 0)
        self.assertEqual(self.spawn.children, [])

    def test_start_runs_the_runner_with_settings(self):
        settings = server.parse_settings('{"camera_id": "cam", "sensitivity": 35}')
        self.guard.start(settings)
        child = self.spawn.daemons()[0]
        self.assertEqual(child.argv, [server.PYTHON, server.RUNNER])
        env = child.kwargs["env"]
        self.assertEqual((env["NG_CAM_ID"], env["NG_SENS"]), ("cam", "35"))
        self.assertEqual(child.kwargs["pass_fds"], (int(env["NG_EVENT_FD"]),))
        self.assertTrue(self.guard.status()["watching"])

    def test_touch_counts_flashes_and_rings(self):
        self.guard.start(dict(server.DEFAULTS))
        child = self.spawn.daemons()[0]
        child.send("ready")
        child.send("touch")
        child.send("touch")
        self.assertTrue(eventually(lambda: self.guard.status()["count"] == 2))
        self.assertEqual(self.guard.status()["last_event"], "touch")
        self.assertEqual(self.spawn.effects().count([server.OVERLAY, "1.2"]), 2)
        self.assertEqual(self.spawn.effects().count([server.AFPLAY, server.ALARM]), 4)

    def test_error_event_and_exit_stop_watching(self):
        self.guard.start(dict(server.DEFAULTS))
        child = self.spawn.daemons()[0]
        child.send("error")
        child.exit(0)
        self.assertTrue(eventually(lambda: self.guard.status()["last_error"] is not None))
        self.assertFalse(self.guard.status()["watching"])
        self.assertIn("camera unavailable", self.guard.status()["last_error"])

    def test_crash_is_reported(self):
        self.guard.start(dict(server.DEFAULTS))
        self.spawn.daemons()[0].exit(1)
        self.assertTrue(eventually(lambda: self.guard.status()["last_error"] is not None))
        self.assertIn("code 1", self.guard.status()["last_error"])

    def test_stop_is_not_an_error(self):
        self.guard.start(dict(server.DEFAULTS))
        child = self.spawn.daemons()[0]
        self.guard.stop()
        self.assertEqual(child.code, -15)
        time.sleep(0.05)
        st = self.guard.status()
        self.assertFalse(st["watching"])
        self.assertIsNone(st["last_error"])

    def test_restart_replaces_the_daemon_and_ignores_the_old_one(self):
        self.guard.start(dict(server.DEFAULTS))
        self.guard.start(dict(server.DEFAULTS, sensitivity=75))
        old, new = self.spawn.daemons()
        self.assertEqual(old.code, -15)
        self.assertIsNone(new.code)
        self.assertEqual(self.guard.status()["sensitivity"], 75)

    def test_reset(self):
        self.guard.count = 5
        self.guard.reset()
        self.assertEqual(self.guard.status()["count"], 0)

    def test_no_overlay_still_rings(self):
        guard = server.Guard(spawn=self.spawn, run=None, later=now, overlay_ok=False)
        guard.alert()
        self.assertEqual(self.spawn.effects(), [[server.AFPLAY, server.ALARM]] * 2)

    def test_cameras(self):
        calls = []

        def run(argv, **kwargs):
            calls.append(argv)
            return subprocess.CompletedProcess(argv, 0, '[{"id": "x", "name": "Cam"}]', "")

        guard = server.Guard(spawn=self.spawn, run=run)
        self.assertEqual(guard.cameras(), [{"id": "x", "name": "Cam"}])
        self.assertEqual(calls[0][-1], "list")
        self.assertEqual(self.spawn.children, [])


class HttpTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.spawn = Spawner()

        def run(argv, **kwargs):
            return subprocess.CompletedProcess(argv, 0, '[{"id": "x", "name": "Cam"}]', "")

        server.guard = server.Guard(spawn=cls.spawn, run=run, later=now, overlay_ok=True)
        cls.httpd = ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        cls.base = "http://127.0.0.1:%d" % cls.httpd.server_address[1]
        threading.Thread(target=cls.httpd.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        server.guard.stop()
        cls.httpd.shutdown()

    def get(self, path):
        with urllib.request.urlopen(self.base + path) as r:
            return json.load(r)

    def post(self, path, body=""):
        request = urllib.request.Request(self.base + path, data=body.encode(), method="POST")
        with urllib.request.urlopen(request) as r:
            return json.load(r)

    def test_health(self):
        self.assertEqual(self.get("/health"), {"ready": True})

    def test_start_touch_reset_stop(self):
        st = self.post("/start", '{"camera_id": "cam", "camera_name": "Cam", "hold": 1}')
        self.assertTrue(st["watching"])
        self.assertEqual((st["camera_name"], st["hold"]), ("Cam", 1))
        self.spawn.daemons()[-1].send("touch")
        self.assertTrue(eventually(lambda: self.get("/status")["count"] == 1))
        self.assertEqual(self.post("/reset")["count"], 0)
        self.assertFalse(self.post("/stop")["watching"])

    def test_bad_start_is_400(self):
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.post("/start", '{"sensitivity": 500}')
        e.exception.close()
        self.assertEqual(e.exception.code, 400)

    def test_cameras(self):
        self.assertEqual(self.get("/cameras"), [{"id": "x", "name": "Cam"}])

    def test_unknown_is_404(self):
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.get("/nope")
        e.exception.close()
        self.assertEqual(e.exception.code, 404)


if __name__ == "__main__":
    unittest.main()
