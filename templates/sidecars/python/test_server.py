"""Tests for server.py: `python3 -m unittest` in this folder."""

import json
import threading
import unittest
import urllib.request
from http.server import ThreadingHTTPServer

import server


class ProcessTest(unittest.TestCase):
    def test_process(self):
        self.assertEqual(server.process("abc"), "ABC")


class HttpTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        server.warm_up()
        cls.httpd = ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        cls.base = "http://127.0.0.1:%d" % cls.httpd.server_address[1]
        threading.Thread(target=cls.httpd.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()

    def test_health(self):
        with urllib.request.urlopen(self.base + "/health") as r:
            self.assertEqual(json.load(r), {"ready": True})

    def test_post_process(self):
        request = urllib.request.Request(self.base + "/process", data="héllo".encode(), method="POST")
        with urllib.request.urlopen(request) as r:
            self.assertEqual(r.read().decode(), "HÉLLO")


if __name__ == "__main__":
    unittest.main()
