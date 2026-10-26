import http.client
import http.server
import json
import socket
import threading
import unittest
from unittest.mock import patch

from webdriver_client import WebDriverClient


class Driver(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_GET(self):
        self.respond()

    def do_POST(self):
        self.respond()

    def respond(self):
        data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.server.requests.append((self.command, self.path, data))
        self.server.connections.add(self.client_address)
        self.server.connection_headers.append(self.headers.get("Connection"))
        # Model a lost response after the operation was dispatched. Retrying a
        # create/broadcast here would silently execute it twice.
        if self.server.drop_next:
            self.server.drop_next = False
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
            return
        payload = json.dumps({"value": self.server.value}).encode()
        self.send_response(self.server.status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


class WebDriverTransport(unittest.TestCase):
    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Driver)
        self.server.requests = []
        self.server.connections = set()
        self.server.connection_headers = []
        self.server.drop_next = False
        self.server.status = 200
        self.server.value = {"sessionId": "test"}
        self.worker = threading.Thread(target=self.server.serve_forever,
                                       kwargs={"poll_interval": 0.01}, daemon=True)
        self.worker.start()
        self.client = WebDriverClient("127.0.0.1", self.server.server_port, timeout=2)

    def tearDown(self):
        self.client.close()
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(timeout=2)
        self.assertFalse(self.worker.is_alive())

    def test_commands_reuse_one_connection_without_forcing_webkit_closed(self):
        self.assertEqual(self.client.request("POST", "/session", {}), {"sessionId": "test"})
        body = {"script": "return document.readyState", "args": []}
        for _ in range(20):
            self.assertEqual(self.client.request("POST", "/session/test/execute/sync", body),
                             {"sessionId": "test"})
        self.assertEqual(len(self.server.requests), 21)
        self.assertEqual(len(self.server.connections), 1)
        self.assertNotIn("close", self.server.connection_headers)
        self.assertEqual(json.loads(self.server.requests[-1][2]), body)

    def test_lost_command_responses_never_replay_wallet_mutations(self):
        for command in ("create_wallet", "broadcast"):
            with self.subTest(command=command):
                before = len(self.server.requests)
                self.server.drop_next = True
                with self.assertRaises(http.client.RemoteDisconnected):
                    self.client.request("POST", "/session/test/execute/async", {"command": command})
                self.assertEqual(len(self.server.requests), before + 1)
                # A later explicit request may reconnect; the lost command is
                # still present exactly once and is never replayed implicitly.
                self.client.request("GET", "/status")
                self.assertEqual(len(self.server.requests), before + 2)
                self.assertEqual(self.server.requests[-1][:2], ("GET", "/status"))

    def test_startup_owner_can_retry_after_connection_refusal(self):
        with patch.object(self.client.connection, "connect", side_effect=ConnectionRefusedError):
            with self.assertRaises(ConnectionRefusedError):
                self.client.request("POST", "/session", {})
        self.assertEqual(self.server.requests, [])
        self.assertEqual(self.client.request("POST", "/session", {}), {"sessionId": "test"})

    def test_http_error_is_not_retried_and_its_body_is_fully_consumed(self):
        self.server.status = 500
        self.server.value = {"error": "unknown error", "message": "x" * 1000}
        with self.assertRaisesRegex(RuntimeError, "WebDriver POST /session"):
            self.client.request("POST", "/session", {})
        self.assertEqual(len(self.server.requests), 1)
        self.server.status = 200
        self.server.value = {"ready": True}
        self.assertEqual(self.client.request("GET", "/status"), {"ready": True})
        self.assertEqual(len(self.server.connections), 1)

    def test_false_and_null_webdriver_values_are_preserved(self):
        self.server.value = False
        self.assertIs(self.client.request("GET", "/status"), False)
        self.server.value = None
        self.assertIsNone(self.client.request("GET", "/status"))


if __name__ == "__main__":
    unittest.main()
