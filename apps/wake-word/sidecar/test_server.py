"""Tests for server.py: `python3 -m unittest` in this folder. Nothing opens
the microphone: the capture child, the HTTP calls and the clock are fakes,
and the capture file is written by the test."""

import json
import os
import struct
import tempfile
import unittest

import server


class RulesTest(unittest.TestCase):
    def test_match(self):
        self.assertTrue(server.match("Hey, Kodak, what's up"))
        self.assertTrue(server.match("CODEC"))
        self.assertFalse(server.match("the code compiles"))
        self.assertFalse(server.match(None))

    def test_window(self):
        self.assertIsNone(server.window(1000))
        size = server.BYTES_PER_S * 10 + 1
        offset, length = server.window(size)
        self.assertEqual(length, server.BYTES_PER_S * 4)
        self.assertEqual(offset % 2, 0)
        self.assertEqual(server.window(server.BYTES_PER_S * 2), (0, server.BYTES_PER_S * 2))

    def test_preroll(self):
        self.assertIsNone(server.preroll(100, 100))
        self.assertEqual(server.preroll(1000, 11), (10, 988))
        cap = server.BYTES_PER_S * server.PREROLL_MAX_S
        self.assertEqual(server.preroll(cap * 3, 0), (0, cap))

    def test_utterance(self):
        self.assertIsNone(server.utterance("Codec."))
        self.assertIsNone(server.utterance("  "))
        self.assertEqual(server.utterance("What time is it, Codec?"), "What time is it, Codec?")

    def test_mic_device(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "node.toml")
            self.assertEqual(server.mic_device(path), "default")
            with open(path, "w") as f:
                f.write('[mic]\ndevice = "MacBook Pro Microphone"\n')
            self.assertEqual(server.mic_device(path), "MacBook Pro Microphone")
            with open(path, "w") as f:
                f.write("[tts]\nvoice = \"alba\"\n")
            self.assertEqual(server.mic_device(path), "default")
            with open(path, "w") as f:
                f.write("not = toml = at all")
            self.assertEqual(server.mic_device(path), "default")

    def test_rms(self):
        self.assertEqual(server.rms(b""), 0.0)
        loud = struct.pack("<4h", 1000, -1000, 1000, -1000)
        self.assertAlmostEqual(server.rms(loud, stride=1), 1000.0)

    def test_wav_header(self):
        h = server.wav_header(32000)
        self.assertEqual(len(h), 44)
        self.assertEqual(h[:4], b"RIFF")
        self.assertEqual(struct.unpack("<I", h[40:44])[0], 32000)

    def test_daemon_state(self):
        self.assertEqual(server.daemon_state(0, ""), "down")
        self.assertEqual(server.daemon_state(200, '{"running": true}'), "live")
        self.assertEqual(server.daemon_state(200, '{"running": false, "agent": "down"}'), "agent_down")
        self.assertEqual(server.daemon_state(200, '{"running": false, "agent": "up"}'), "idle")
        self.assertEqual(server.daemon_state(200, "not json"), "down")

    def test_timing(self):
        self.assertTrue(server.fire(100, None))
        self.assertFalse(server.fire(105, 100))
        self.assertTrue(server.fire(108, 100))
        self.assertFalse(server.settled(103, 100))
        self.assertTrue(server.settled(106, 100))

    def test_why(self):
        self.assertEqual(server.why(False, "idle", True), "off")
        self.assertEqual(server.why(True, "live", True), "paused, call in progress")
        self.assertEqual(server.why(True, "idle", True), "listening")


class FakeChild:
    def __init__(self, argv, **_):
        self.argv, self.alive = argv, True

    def poll(self):
        return None if self.alive else 0

    def terminate(self):
        self.alive = False

    def wait(self, timeout=None):
        return 0

    def kill(self):
        self.alive = False


class ListenerTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        server.RAW = os.path.join(self.dir.name, "wake.raw")
        server.WAV = os.path.join(self.dir.name, "window.wav")
        server.PREROLL_WAV = os.path.join(self.dir.name, "preroll.wav")
        self.calls = []
        self.answers = {}
        self.now = 1000.0
        self.listener = server.Listener(spawn=FakeChild, call=self.call, clock=lambda: self.now)

    def tearDown(self):
        self.dir.cleanup()

    def call(self, method, url, body=b"", headers=None):
        self.calls.append((method, url, body))
        for suffix, answer in self.answers.items():
            if url.endswith(suffix):
                return answer(body) if callable(answer) else answer
        return 0, ""

    def speak(self, seconds=5, amplitude=2000):
        frames = int(seconds * server.RATE)
        with open(server.RAW, "wb") as f:
            f.write(struct.pack("<%dh" % frames, *([amplitude, -amplitude] * (frames // 2))))

    def test_enable_spawns_capture_on_the_device(self):
        self.listener.enable("MacBook Pro Microphone")
        self.assertIn(":MacBook Pro Microphone", self.listener.child.argv)
        self.listener.disable()
        self.assertIsNone(self.listener.child)

    def test_word_starts_a_call_and_sends_the_preroll(self):
        self.answers = {
            "/status": (200, '{"running": false, "agent": "up"}'),
            "/transcribe": (200, "What time is it, Codec?"),
            "/start": (200, '{"running": true}'),
            "/heard": (200, "{}"),
        }
        self.listener.enable("0")
        self.speak()
        self.listener.tick()
        urls = [u for _, u, _ in self.calls]
        self.assertTrue(any(u.endswith("/start") for u in urls))
        heard = [b for _, u, b in self.calls if u.endswith("/heard")]
        self.assertEqual(json.loads(heard[0]), {"text": "What time is it, Codec?"})

    def test_quiet_room_transcribes_nothing(self):
        self.answers = {"/status": (200, '{"running": false}')}
        self.listener.enable("0")
        self.speak(amplitude=20)
        self.listener.tick()
        self.assertFalse(any(u.endswith("/transcribe") for _, u, _ in self.calls))

    def test_no_wake_during_a_call_or_just_after(self):
        self.answers = {"/status": (200, '{"running": true}'), "/transcribe": (200, "Codec")}
        self.listener.enable("0")
        self.speak()
        self.listener.tick()
        self.answers["/status"] = (200, '{"running": false}')
        self.now += 1
        self.listener.tick()
        self.assertFalse(any(u.endswith("/transcribe") for _, u, _ in self.calls))
        self.now += server.SETTLE_S
        self.listener.tick()
        self.assertTrue(any(u.endswith("/transcribe") for _, u, _ in self.calls))

    def test_disabled_does_nothing(self):
        self.listener.tick()
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
