"""Tests for funnel.py, the pure half: `python3 -m unittest` in this folder.
Most cases come from tests/spec/tailscale_funnel_spec.lua in the Hammerspoon
setup."""

import json
import unittest

import funnel as F

HOST = "pm-mac.yak-bebop.ts.net"

# `tailscale status --json`, trimmed to the keys this sidecar reads. The
# capability values really are null.
STATUS = json.dumps(
    {
        "Version": "1.78.1",
        "BackendState": "Running",
        "MagicDNSSuffix": "yak-bebop.ts.net",
        "Self": {
            "HostName": "pm-mac",
            "DNSName": HOST + ".",
            "Online": True,
            "CapMap": {"https://tailscale.com/cap/funnel": None, "https://tailscale.com/cap/ssh": None},
        },
    }
)
LOGGED_OUT = '{"Version":"1.78.1","BackendState":"NeedsLogin","Self":{"CapMap":{}}}'
STOPPED = '{"Version":"1.78.1","BackendState":"Stopped","Self":{"DNSName":"%s.","CapMap":{}}}' % HOST
NO_FUNNEL_CAP = json.dumps(
    {"BackendState": "Running", "Self": {"DNSName": HOST + ".", "CapMap": {"https://tailscale.com/cap/ssh": None}}}
)


def serve_on(port):
    return json.dumps(
        {
            "TCP": {"443": {"HTTPS": True}},
            "Web": {HOST + ":443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:%d" % port}}}},
            "AllowFunnel": {HOST + ":443": True},
        }
    )


SERVE_ON = serve_on(8794)


class CommandsTest(unittest.TestCase):
    def test_read_only_commands(self):
        self.assertEqual(F.status_command(), ["status", "--json"])
        self.assertEqual(F.serve_command(), ["serve", "status", "--json"])

    def test_arms_in_the_background_on_443(self):
        self.assertEqual(F.on_command(8794), ["funnel", "--bg", "--https=443", "http://127.0.0.1:8794"])

    def test_turns_off_on_the_same_port(self):
        self.assertEqual(F.off_command(), ["funnel", "--https=443", "off"])

    def test_origin(self):
        self.assertEqual(F.origin(9000), "http://127.0.0.1:9000")
        self.assertEqual(F.origin("9000"), "http://127.0.0.1:9000")

    def test_find_bin_prefers_the_standalone_app(self):
        there = {"/opt/homebrew/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"}
        self.assertEqual(F.find_bin(there.__contains__), "/Applications/Tailscale.app/Contents/MacOS/Tailscale")
        self.assertEqual(F.find_bin({"/opt/homebrew/bin/tailscale"}.__contains__), "/opt/homebrew/bin/tailscale")
        self.assertIsNone(F.find_bin(lambda _: False))


class ParseStatusTest(unittest.TestCase):
    def test_hostname_without_the_trailing_dot(self):
        s = F.parse_status(STATUS)
        self.assertEqual(s["hostname"], HOST)
        self.assertEqual(s["backend"], "Running")
        self.assertTrue(s["online"])

    def test_sees_the_funnel_capability_with_a_null_value(self):
        self.assertIs(F.parse_status(STATUS)["funnel"], True)

    def test_capability_missing(self):
        self.assertIs(F.parse_status(NO_FUNNEL_CAP)["funnel"], False)

    def test_cannot_tell_without_capabilities(self):
        self.assertIsNone(F.parse_status('{"BackendState":"Running","Self":{"DNSName":"x.ts.net."}}')["funnel"])

    def test_capabilities_list(self):
        doc = '{"BackendState":"Running","Self":{"Capabilities":["https://tailscale.com/cap/funnel"]}}'
        self.assertIs(F.parse_status(doc)["funnel"], True)

    def test_not_json(self):
        self.assertIsNone(F.parse_status("command not found"))
        self.assertIsNone(F.parse_status(None))

    def test_auth_url(self):
        doc = '{"BackendState":"NeedsLogin","AuthURL":"https://login.tailscale.com/a/abc123"}'
        self.assertEqual(F.parse_status(doc)["auth_url"], "https://login.tailscale.com/a/abc123")


class StatusProblemTest(unittest.TestCase):
    def test_none_when_logged_in_named_and_allowed(self):
        self.assertIsNone(F.status_problem(F.parse_status(STATUS)))

    def test_log_in(self):
        self.assertEqual(F.status_problem(F.parse_status(LOGGED_OUT)), F.NOT_LOGGED_IN)

    def test_stopped(self):
        self.assertEqual(F.status_problem(F.parse_status(STOPPED)), F.STOPPED)

    def test_no_name_yet(self):
        self.assertEqual(F.status_problem({"backend": "Running"}), F.NO_HOSTNAME)

    def test_not_allowed(self):
        self.assertEqual(F.status_problem(F.parse_status(NO_FUNNEL_CAP)), F.NOT_ALLOWED)

    def test_starting_gets_the_benefit_of_the_doubt(self):
        self.assertIsNone(F.status_problem({"backend": "Starting", "hostname": HOST}))

    def test_nothing_answered(self):
        self.assertEqual(F.status_problem(None), F.NO_STATUS)


class ParseServeTest(unittest.TestCase):
    def test_live_funnel(self):
        s = F.parse_serve(SERVE_ON)
        self.assertTrue(s["on"])
        self.assertEqual(s["host"], HOST)
        self.assertEqual(s["funnel_port"], 443)
        self.assertEqual(s["target"], "http://127.0.0.1:8794")
        self.assertEqual(s["port"], 8794)

    def test_off_however_it_says_so(self):
        for text in ("{}", "", "No serve config\n", None):
            self.assertFalse(F.parse_serve(text)["on"], text)

    def test_open_with_nothing_behind_it_is_off(self):
        self.assertFalse(F.parse_serve('{"AllowFunnel":{"%s:443":true},"Web":{}}' % HOST)["on"])

    def test_served_but_not_open_is_off(self):
        doc = '{"Web":{"%s:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:8794"}}}}}' % HOST
        self.assertFalse(F.parse_serve(doc)["on"])

    def test_human_readable_form(self):
        s = F.parse_serve("https://%s (Funnel on)\n|-- / proxy http://127.0.0.1:8794\n" % HOST)
        self.assertTrue(s["on"])
        self.assertEqual(s["host"], HOST)
        self.assertEqual(s["port"], 8794)


class PortProblemTest(unittest.TestCase):
    def test_somebody_elses_port(self):
        why = F.port_problem(F.parse_serve(serve_on(9999)), 8794)
        self.assertIn("9999", why)
        self.assertIn("8794", why)

    def test_quiet(self):
        self.assertIsNone(F.port_problem(F.parse_serve(SERVE_ON), 8794))
        self.assertIsNone(F.port_problem(F.parse_serve("{}"), 8794))
        self.assertIsNone(F.port_problem(F.parse_serve(SERVE_ON), None))


class FatalReasonTest(unittest.TestCase):
    def test_complaints(self):
        self.assertEqual(F.fatal_reason("Funnel is not enabled on your tailnet.\nadd it to nodeAttrs"), F.NOT_ALLOWED)
        self.assertEqual(F.fatal_reason("failed to connect to local tailscaled; is tailscaled running?"), F.NOT_RUNNING)
        self.assertEqual(F.fatal_reason("Logged out."), F.NOT_LOGGED_IN)
        self.assertEqual(F.fatal_reason("error: HTTPS must be enabled"), F.NO_HTTPS)
        self.assertEqual(F.fatal_reason("Tailscale is stopped."), F.STOPPED)

    def test_ordinary_output(self):
        self.assertIsNone(F.fatal_reason('{"BackendState":"Running"}'))
        self.assertIsNone(F.fatal_reason(42))


class RequestTest(unittest.TestCase):
    def test_bad_request(self):
        self.assertEqual(F.bad_request(F.normalize({"mode": "funnel", "port": 1})), "owner is required")
        self.assertIn("port", F.bad_request(F.normalize({"owner": "a", "mode": "funnel"})))
        self.assertIsNone(F.bad_request(F.normalize({"owner": "a", "mode": "off"})))
        self.assertTrue(F.bad_request(F.normalize({"owner": "a", "mode": "quick", "port": 1})))

    def test_normalize(self):
        self.assertEqual(F.normalize({"owner": "a"})["mode"], "funnel")
        self.assertEqual(F.normalize({"public_url": "https://x.ts.net//"})["public_url"], "https://x.ts.net")
        self.assertEqual(F.normalize({})["ttl_s"], F.LEASE_TTL_S)
        self.assertEqual(F.normalize({"ttl_s": 0})["ttl_s"], F.LEASE_TTL_S)
        self.assertEqual(F.normalize({"ttl_s": "45"})["ttl_s"], 45)
        self.assertIsNone(F.normalize({"probe_path": ""})["probe_path"])

    def test_json_body(self):
        r = F.request('{"owner":"voice","mode":"funnel","port":8794}')
        self.assertEqual((r["owner"], r["port"]), ("voice", 8794))

    def test_form_body(self):
        self.assertEqual(F.request("owner=voice&mode=funnel&port=1")["owner"], "voice")

    def test_query_string(self):
        r = F.request("", F.params("/lease?owner=voice&port=8794"))
        self.assertEqual((r["owner"], r["port"]), ("voice", 8794))

    def test_body_wins(self):
        self.assertEqual(F.request('{"owner":"body"}', {"owner": "query"})["owner"], "body")


class LeasesTest(unittest.TestCase):
    def test_expire(self):
        leases = {"a": {"expires": 10}, "b": {"expires": 50}, "c": {"expires": 5}}
        self.assertEqual(F.expire(leases, 20), ["a", "c"])
        self.assertEqual(list(leases), ["b"])

    def test_primary_is_the_first_taken(self):
        leases = {
            "late": {"expires": 99, "taken": 2, "req": {"port": 2}},
            "early": {"expires": 99, "taken": 1, "req": {"port": 1}},
        }
        req, owner = F.primary(leases, 0)
        self.assertEqual((owner, req["port"]), ("early", 1))
        self.assertEqual(F.primary({}, 0), (None, None))

    def test_active_list_by_owner(self):
        leases = {"zed": {"expires": 110}, "amy": {"expires": 130}}
        self.assertEqual(
            F.active_list(leases, 100), [{"owner": "amy", "expires_in": 30}, {"owner": "zed", "expires_in": 10}]
        )

    def test_expires_exactly_now_is_gone(self):
        self.assertEqual(F.active_list({"amy": {"expires": 100}}, 100), [])
        self.assertEqual(F.active_count({"amy": {"expires": 100}}, 100), 0)

    def test_conflict(self):
        self.assertTrue(F.conflict({"port": 8794, "mode": "funnel"}, {"port": 9000, "mode": "funnel"}))
        self.assertIsNone(F.conflict({"port": 8794, "mode": "funnel"}, {"port": 8794, "mode": "funnel"}))


class StatusTest(unittest.TestCase):
    def test_compute_status(self):
        self.assertEqual(F.compute_status({"fatal": True, "funnel": True, "url": "https://x"}), "error")
        self.assertEqual(F.compute_status({"url": "https://x", "funnel": True}), "ready")
        self.assertEqual(F.compute_status({"url": "https://x"}), "off")
        self.assertEqual(F.compute_status({"pending": True}), "starting")
        self.assertEqual(F.compute_status({}), "off")
        self.assertEqual(F.compute_status({"mode": "off", "funnel": True, "url": "https://x"}), "off")

    def test_every_key_always(self):
        text = F.encode_state(F.snapshot({"generation": 0, "leases": {}}, 100))
        doc = json.loads(text)
        self.assertEqual(list(doc), F.STATE_KEYS)
        self.assertIn('"error":null', text)
        self.assertIn('"url":null', text)
        self.assertIn('"funnel":false', text)
        self.assertIn('"leases":[]', text)

    def test_round_trip(self):
        snap = F.snapshot(
            {"url": "https://" + HOST, "hostname": HOST, "funnel": True, "generation": 4,
             "mode": "funnel", "port": 8794, "started_at": 40, "leases": {}},
            100,
        )
        back = json.loads(F.encode_state(snap))
        self.assertEqual((back["status"], back["hostname"], back["generation"]), ("ready", HOST, 4))
        self.assertEqual(back["uptime_s"], 60)
        self.assertIs(back["funnel"], True)

    def test_probe_ok(self):
        self.assertTrue(F.probe_ok("whatever", None))
        self.assertTrue(F.probe_ok(None, ""))
        self.assertTrue(F.probe_ok("  tok-9 \n", "tok-9"))
        self.assertFalse(F.probe_ok("tok-8", "tok-9"))


class TileTextTest(unittest.TestCase):
    def test_status_line(self):
        ready = {"url": "https://x", "funnel": True, "generation": 3, "started_at": 0}
        self.assertEqual(F.status_line(ready, 125), "On · up 2m 5s · generation 3")
        self.assertNotIn("https://x", F.status_line(ready, 125))
        self.assertEqual(F.status_line({"fatal": True, "err": F.STOPPED}, 0), "Error: " + F.STOPPED)
        waiting = {"leases": {"voice": {"expires": 99}}}
        self.assertEqual(F.status_line(waiting, 0), "Off: " + F.FUNNEL_OFF)
        self.assertEqual(F.status_line({"mode": "off", "url": "https://own"}, 0), "Off: the lease supplies https://own")
        self.assertEqual(F.status_line({}, 0), "Off: nothing has leased the funnel")

    def test_url_line_and_copyable(self):
        self.assertEqual(F.url_line({"url": "https://x"}), "https://x")
        self.assertEqual(F.url_line({"hostname": HOST}), "https://%s (funnel off)" % HOST)
        self.assertEqual(F.url_line({}), F.NO_URL)
        self.assertEqual(F.copyable({"hostname": HOST}), "https://" + HOST)
        self.assertIsNone(F.copyable({}))

    def test_human_duration(self):
        self.assertEqual(F.human_duration(5), "5s")
        self.assertEqual(F.human_duration(65), "1m 5s")
        self.assertEqual(F.human_duration(3725), "1h 2m")

    def test_lease_line(self):
        self.assertEqual(F.lease_line([]), "No leases")
        self.assertEqual(F.lease_line([{"owner": "voice-agent", "expires_in": 90}]), "Leased by voice-agent (1m 30s left)")


class SetupTest(unittest.TestCase):
    def steps(self, **env):
        return {s["key"]: s for s in F.setup_steps(env)}

    def test_client_first(self):
        s = self.steps()
        self.assertEqual((s["install"]["state"], s["install"]["action"]), ("manual", "install"))
        self.assertEqual(s["signin"]["state"], "wait")
        self.assertEqual(s["arm"]["state"], "wait")

    def test_stopped_opens_the_app(self):
        s = self.steps(bin="/x", status=F.parse_status(STOPPED))
        self.assertEqual((s["signin"]["state"], s["signin"]["action"]), ("manual", "openapp"))

    def test_logged_out_signs_in(self):
        s = self.steps(bin="/x", status=F.parse_status(LOGGED_OUT))
        self.assertEqual(s["signin"]["action"], "signin")

    def test_machine_approval(self):
        s = self.steps(bin="/x", status={"backend": "NeedsMachineAuth"})
        self.assertEqual(s["signin"]["action"], "adminmachines")

    def test_tailnet_switches(self):
        s = self.steps(bin="/x", status={"backend": "Running", "funnel": False})
        self.assertEqual(s["dns"]["action"], "admindns")
        self.assertEqual(s["funnel"]["action"], "adminacl")

    def test_arming_is_last_and_still_a_click(self):
        st = F.parse_status(STATUS)
        s = self.steps(bin="/x", status=st, hostname=HOST, port=8794)
        self.assertEqual((s["arm"]["state"], s["arm"]["action"]), ("todo", "arm"))
        s = self.steps(bin="/x", status=st, hostname=HOST, port=8794, funnel=True, url="https://" + HOST)
        self.assertEqual(s["arm"]["state"], "done")

    def test_meta_is_never_ticked_or_counted(self):
        steps = F.setup_steps({"bin": "/x", "status": F.parse_status(STATUS), "hostname": HOST, "port": 8794,
                               "funnel": True, "url": "https://" + HOST})
        meta = steps[-1]
        self.assertEqual((meta["state"], meta["action"]), ("manual", "copyhook"))
        self.assertIn(HOST + F.WEBHOOK_PATH, meta["detail"])
        self.assertEqual(F.setup_line(steps), "Setup 6 of 6 done")
        self.assertIsNone(F.next_step(steps))

    def test_next_step(self):
        steps = F.setup_steps({"bin": "/x", "status": F.parse_status(STOPPED)})
        self.assertEqual(F.next_step(steps)["key"], "signin")
        self.assertEqual(F.setup_line(steps), "Setup 1 of 6 done")

    def test_setup_text(self):
        self.assertEqual(F.webhook_url(HOST), "https://%s/whatsapp/webhook" % HOST)
        self.assertIsNone(F.webhook_url(None))
        self.assertIn('"attr":   ["funnel"]', F.POLICY_SNIPPET)
        self.assertIn("PHONE_PUBLIC_URL=https://" + HOST, F.env_lines(HOST))
        self.assertIn("PHONE_TUNNEL_PLUGIN=http://127.0.0.1:8795", F.env_lines(None))
        self.assertIn("<machine>.<tailnet>", F.env_lines(None))


if __name__ == "__main__":
    unittest.main()
