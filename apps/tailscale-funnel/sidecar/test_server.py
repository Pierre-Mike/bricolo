"""Tests for server.py: `python3 -m unittest` in this folder. No tailscale
runs and nothing is published: the CLI, the clock, the filesystem and the
probe are fakes, and the HTTP tests serve a Funnel built on them."""

import json
import threading
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

import funnel as F
import server
from test_funnel import HOST, LOGGED_OUT, NO_FUNNEL_CAP, SERVE_ON, STATUS, STOPPED, serve_on

BIN = "/Applications/Tailscale.app/Contents/MacOS/Tailscale"

# The log lines go to the node's sidecar log, not the test output.
server.log = lambda *args: None
server.Handler.log_message = lambda *args: None


class FakeCli:
    """Answers `status --json` and `serve status --json` with what a test
    wrote, records every call, and plays tailscaled for funnel on and off."""

    def __init__(self, status=STATUS, serve="{}"):
        self.status, self.serve = (0, status), (0, serve)
        self.funnel = (0, "")
        self.calls = []

    def __call__(self, binary, args):
        assert binary == BIN, binary
        self.calls.append(args)
        if args == ["status", "--json"]:
            return self.status
        if args == ["serve", "status", "--json"]:
            return self.serve
        if args[0] == "funnel":
            code, out = self.funnel
            if code == 0:
                self.serve = (0, serve_on(int(args[-1].rsplit(":", 1)[1])) if args[-1] != "off" else "{}")
            return code, out
        raise AssertionError("unexpected tailscale call %r" % args)

    def published(self):
        return [c for c in self.calls if c[0] != "status" and c[:2] != ["serve", "status"]]


class Clock:
    def __init__(self):
        self.t = 1000.0

    def __call__(self):
        return self.t


def make(cli=None, installed=True, fetch=None):
    cli = cli or FakeCli()
    clock = Clock()
    f = server.Funnel(
        run=cli,
        exists=lambda p: installed and p == BIN,
        now=clock,
        fetch=fetch or (lambda url: (200, "")),
    )
    return f, cli, clock


def take(f, **req):
    return f.lease(F.normalize({"owner": "voice", "mode": "funnel", "port": 8794, **req}))


class FunnelTest(unittest.TestCase):
    def test_reads_who_this_mac_is_and_what_it_serves_and_nothing_else(self):
        f, cli, _ = make()
        take(f)
        self.assertEqual(cli.calls, [["status", "--json"], ["serve", "status", "--json"]])

    def test_hands_out_the_url_once_the_funnel_carries_the_leased_port(self):
        f, _, _ = make(FakeCli(serve=SERVE_ON))
        snap, code = take(f)
        self.assertEqual(code, 200)
        self.assertEqual((snap["status"], snap["url"], snap["generation"]), ("ready", "https://" + HOST, 1))
        self.assertIsNone(snap["error"])
        self.assertEqual(snap["leases"], [{"owner": "voice", "expires_in": 120}])

    def test_no_url_while_the_funnel_is_off_and_says_why(self):
        f, cli, _ = make()
        snap, code = take(f)
        self.assertEqual((code, snap["status"], snap["url"], snap["hostname"]), (200, "off", None, HOST))
        self.assertEqual(snap["error"], F.FUNNEL_OFF)
        self.assertEqual(cli.published(), [])

    def test_stopped_tailscale_is_an_error_with_no_url(self):
        f, cli, _ = make(FakeCli(status=STOPPED))
        snap, code = take(f)
        self.assertEqual((code, snap["status"], snap["url"]), (503, "error", None))
        self.assertEqual(snap["error"], F.STOPPED)
        self.assertEqual(cli.published(), [])

    def test_a_route_kept_while_stopped_is_not_a_live_funnel(self):
        f, _, _ = make(FakeCli(status=STOPPED, serve=SERVE_ON))
        snap, _ = take(f)
        self.assertEqual((snap["status"], snap["funnel"], snap["url"], snap["generation"]), ("error", False, None, 0))

    def test_logged_out_and_not_allowed(self):
        f, _, _ = make(FakeCli(status=LOGGED_OUT))
        self.assertEqual(take(f)[0]["error"], F.NOT_LOGGED_IN)
        f, _, _ = make(FakeCli(status=NO_FUNNEL_CAP))
        self.assertEqual(take(f)[0]["error"], F.NOT_ALLOWED)

    def test_tailscaled_not_answering(self):
        cli = FakeCli()
        cli.status = (1, "failed to connect to local tailscaled; is tailscaled running?")
        f, _, _ = make(cli)
        self.assertEqual(take(f)[0]["error"], F.NOT_RUNNING)

    def test_not_installed_and_runs_nothing(self):
        f, cli, _ = make(installed=False)
        snap, code = take(f)
        self.assertEqual((code, snap["error"]), (503, F.NOT_INSTALLED))
        self.assertEqual(cli.calls, [])

    def test_refuses_a_url_for_a_funnel_pointed_elsewhere(self):
        f, _, _ = make(FakeCli(serve=serve_on(9999)))
        snap, _ = take(f)
        self.assertEqual(snap["status"], "error")
        self.assertIsNone(snap["url"])
        self.assertIn("9999", snap["error"])

    def test_clears_a_fault_by_itself_once_fixed(self):
        cli = FakeCli(status=LOGGED_OUT, serve=SERVE_ON)
        f, _, _ = make(cli)
        self.assertEqual(take(f)[0]["status"], "error")
        cli.status = (0, STATUS)
        f.ask()
        self.assertEqual(f.state()["status"], "ready")

    def test_answers_a_renewal_without_asking_again(self):
        f, cli, _ = make()
        take(f)
        take(f)
        self.assertEqual(len(cli.calls), 2)

    def test_second_owner_same_port_shares_it(self):
        f, _, _ = make(FakeCli(serve=SERVE_ON))
        take(f)
        snap, code = f.lease(F.normalize({"owner": "other", "mode": "funnel", "port": 8794}))
        self.assertEqual((code, snap["url"]), (200, "https://" + HOST))
        self.assertEqual([l["owner"] for l in snap["leases"]], ["other", "voice"])

    def test_refuses_a_second_owner_on_another_port(self):
        f, _, _ = make(FakeCli(serve=SERVE_ON))
        take(f)
        snap, code = f.lease(F.normalize({"owner": "other", "mode": "funnel", "port": 9000}))
        self.assertEqual(code, 409)
        self.assertIn("port 9000", snap["error"])
        self.assertEqual([l["owner"] for l in f.state()["leases"]], ["voice"])

    def test_off_lease_runs_nothing_and_shows_its_url(self):
        f, cli, _ = make()
        snap, _ = f.lease(F.normalize({"owner": "x", "mode": "off", "public_url": "https://own.example/"}))
        self.assertEqual((snap["status"], snap["url"], snap["mode"]), ("off", "https://own.example", "off"))
        self.assertEqual(cli.calls, [])

    def test_leaves_the_funnel_up_when_the_last_lease_goes(self):
        f, cli, clock = make(FakeCli(serve=SERVE_ON))
        take(f)
        clock.t += 121
        f.watch()
        snap = f.state()
        self.assertEqual(snap["leases"], [])
        self.assertTrue(snap["funnel"])
        self.assertEqual(cli.published(), [])

    def test_release(self):
        f, cli, _ = make(FakeCli(serve=SERVE_ON))
        take(f)
        snap = f.release("voice")
        self.assertEqual(snap["leases"], [])
        self.assertTrue(snap["funnel"])
        self.assertEqual(cli.published(), [])

    def test_rereads_on_every_tick_while_a_lease_is_live(self):
        f, cli, _ = make()
        take(f)
        f.watch()
        self.assertEqual(len(cli.calls), 4)

    def test_idle_polls_every_tenth_tick(self):
        f, cli, _ = make()
        for _ in range(9):
            f.watch()
        self.assertEqual(cli.calls, [])
        f.watch()
        self.assertEqual(len(cli.calls), 2)

    def test_notices_a_funnel_turned_off_elsewhere(self):
        cli = FakeCli(serve=SERVE_ON)
        f, _, _ = make(cli)
        take(f)
        cli.serve = (0, "{}")
        f.watch()
        self.assertEqual((f.state()["status"], f.state()["url"]), ("off", None))

    def test_new_generation_when_it_comes_back(self):
        cli = FakeCli(serve=SERVE_ON)
        f, _, _ = make(cli)
        take(f)
        cli.serve = (0, "{}")
        f.ask()
        cli.serve = (0, SERVE_ON)
        f.ask()
        self.assertEqual(f.state()["generation"], 2)

    def test_arms_only_when_asked_on_the_leased_port(self):
        f, cli, _ = make()
        take(f)
        self.assertEqual(cli.published(), [])
        self.assertEqual(f.turn_on(), 200)
        self.assertEqual(cli.published(), [["funnel", "--bg", "--https=443", "http://127.0.0.1:8794"]])
        self.assertEqual((f.state()["status"], f.state()["url"]), ("ready", "https://" + HOST))

    def test_will_not_arm_with_nothing_behind_it(self):
        f, cli, _ = make()
        self.assertEqual(f.turn_on(), 409)
        self.assertEqual(cli.published(), [])

    def test_says_what_the_tailnet_is_missing_when_arming_fails(self):
        cli = FakeCli()
        cli.funnel = (1, "Funnel is not enabled on your tailnet.")
        f, _, _ = make(cli)
        take(f)
        self.assertEqual(f.turn_on(), 502)
        self.assertEqual(f.state()["error"], F.NOT_ALLOWED)

    def test_turns_off_on_request(self):
        f, cli, _ = make(FakeCli(serve=SERVE_ON))
        take(f)
        self.assertEqual(f.turn_off(), 200)
        self.assertEqual(cli.published(), [["funnel", "--https=443", "off"]])
        self.assertFalse(f.state()["funnel"])

    def test_probe_counts_misses_and_recovers(self):
        answers = []
        f, _, _ = make(FakeCli(serve=SERVE_ON), fetch=lambda url: answers.pop(0))
        take(f, probe_path="/phone/ping", probe_expect="tok")
        answers += [(200, "wrong"), (502, ""), (None, "")]
        for _ in range(3):
            f.probe()
        snap = f.state()
        self.assertEqual((snap["status"], snap["error"]), ("error", F.PROBE_DEAD))
        self.assertEqual(snap["url"], "https://" + HOST, "the address still exists")
        answers.append((200, "tok\n"))
        f.probe()
        self.assertEqual(f.state()["status"], "ready")

    def test_no_probe_without_a_probe_path(self):
        f, _, _ = make(FakeCli(serve=SERVE_ON), fetch=lambda url: self.fail("probed"))
        take(f)
        f.probe()

    def test_view(self):
        f, _, _ = make(FakeCli(status=STOPPED))
        take(f)
        v = f.view()
        for key in F.STATE_KEYS:
            self.assertIn(key, v)
        self.assertEqual(v["status_line"], "Error: " + F.STOPPED)
        self.assertEqual(v["url_line"], "https://%s (funnel off)" % HOST)
        self.assertEqual(v["next_step"]["action"], "openapp")
        self.assertEqual(v["next_step"]["button"], "Open the Tailscale app")
        self.assertEqual(v["lease_line"], "Leased by voice (2m 0s left)")
        self.assertEqual(v["owner"], "voice")


class RouteTest(unittest.TestCase):
    def test_bad_lease_without_calling_through(self):
        f, cli, _ = make()
        code, text = server.route(f, "POST", "/lease", '{"mode":"funnel","port":1}')
        self.assertEqual((code, json.loads(text)), (400, {"error": "owner is required"}))
        self.assertEqual(f.state()["leases"], [])

    def test_unknown(self):
        f, _, _ = make()
        code, text = server.route(f, "GET", "/nope", "")
        self.assertEqual(code, 404)
        self.assertIn("/nope", json.loads(text)["error"])

    def test_arm_is_post_only(self):
        f, cli, _ = make()
        take(f)
        self.assertEqual(server.route(f, "GET", "/arm", "")[0], 405)
        self.assertEqual(server.route(f, "GET", "/disarm", "")[0], 405)
        self.assertEqual(cli.published(), [])


class HttpTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.funnel, cls.cli, cls.clock = make(FakeCli(status=STOPPED))
        server.Handler.funnel = cls.funnel
        cls.httpd = ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        cls.base = "http://127.0.0.1:%d" % cls.httpd.server_address[1]
        threading.Thread(target=cls.httpd.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()

    def request(self, path, body=None):
        data = body.encode() if body is not None else None
        try:
            with urllib.request.urlopen(urllib.request.Request(self.base + path, data=data)) as r:
                return r.status, json.loads(r.read())
        except urllib.error.HTTPError as e:
            with e:
                return e.code, json.loads(e.read())

    def test_health(self):
        self.assertEqual(self.request("/health"), (200, {"ready": True}))

    def test_lease_funnel_release_round_trip(self):
        code, snap = self.request("/lease", '{"owner":"test","mode":"funnel","port":8794}')
        self.assertEqual((code, snap["status"], snap["error"]), (503, "error", F.STOPPED))
        self.assertEqual(list(snap), F.STATE_KEYS)
        code, snap = self.request("/funnel")
        self.assertEqual([l["owner"] for l in snap["leases"]], ["test"])
        self.assertEqual(self.request("/tunnel"), (code, snap))
        code, snap = self.request("/release?owner=test")
        self.assertEqual((code, snap["leases"]), (200, []))
        self.assertEqual(self.cli.published(), [])

    def test_lease_as_a_bodyless_get(self):
        code, snap = self.request("/lease?owner=get&port=8794")
        self.assertIn("get", [l["owner"] for l in snap["leases"]])
        self.request("/release", "owner=get")

    def test_view(self):
        code, view = self.request("/view")
        self.assertEqual(code, 200)
        self.assertIn("setup_line", view)


if __name__ == "__main__":
    unittest.main()
