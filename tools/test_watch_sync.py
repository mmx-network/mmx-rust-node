"""Exercise the monitor's completion/rejection gates over real local HTTP."""
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from urllib.parse import parse_qs, urlparse


class WatchSync(unittest.TestCase):
    def run_monitor(self, mode):
        calls = 0

        def block_hash(height):
            return height.to_bytes(32, "big")

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, value):
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                nonlocal calls
                query = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if query["method"] == "get_status":
                    calls += 1
                    height = 100 + (calls - 1 if mode == "advancing" else 0)
                    result = dict(height=height, hash=block_hash(height).hex(), synced=True,
                                  sync_peers=2 if mode == "two_peers" else 3)
                elif query["method"] == "get_genesis_hash":
                    result = list(block_hash(0))
                else:
                    result = list(block_hash(query["params"]["height"]))
                self.reply(dict(jsonrpc="2.0", id=1, result=result))

            def do_GET(self):
                url = urlparse(self.path)
                if url.path.endswith("/node/info"):
                    height = 100 + (calls - 1 if mode == "advancing" else 0)
                    if mode == "behind":
                        height += 100
                    self.reply(dict(height=height, genesis_hash=block_hash(0).hex(), is_synced=True))
                else:
                    height = int(parse_qs(url.query)["height"][0])
                    value = b"\xff" * 32 if mode == "wrong_hash" else block_hash(height)
                    self.reply(dict(hash=value.hex()))

        with HTTPServer(("127.0.0.1", 0), Handler) as server, tempfile.TemporaryDirectory() as temporary:
            worker = threading.Thread(target=server.serve_forever, daemon=True)
            worker.start()
            try:
                endpoint = f"http://127.0.0.1:{server.server_port}"
                result = subprocess.run(
                    [sys.executable, str(Path(__file__).with_name("watch_sync.py")),
                     "--rpc", endpoint + "/rpc", "--native-wapi", endpoint + "/wapi",
                     "--output", str(Path(temporary) / "samples.jsonl"),
                     "--interval", ".02", "--live-seconds", ".015", "--live-blocks", "2",
                     "--samples", "3"],
                    text=True, capture_output=True, timeout=10,
                )
                rows = [json.loads(line) for line in result.stdout.splitlines()]
                persisted = [json.loads(line) for line in (Path(temporary) / "samples.jsonl").read_text().splitlines()]
                self.assertEqual(rows, persisted)
                return result, rows
            finally:
                server.shutdown()
                worker.join()

    def test_live_requires_elapsed_time_and_actual_advancement(self):
        result, rows = self.run_monitor("advancing")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(rows[-1]["phase"], "live_verified")
        self.assertGreaterEqual(rows[-1]["blocks"], 2)
        self.assertGreaterEqual(rows[-1]["seconds"], .015)

    def test_a_frozen_synced_height_does_not_prove_live_advancement(self):
        result, rows = self.run_monitor("frozen")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(rows[-1]["phase"], "samples_complete")
        self.assertFalse(rows[-1]["live_verified"])

    def test_a_synced_flag_while_behind_native_does_not_prove_catchup(self):
        result, rows = self.run_monitor("behind")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("live_verified", [row["phase"] for row in rows])

    def test_two_peers_cannot_satisfy_the_live_gate(self):
        result, rows = self.run_monitor("two_peers")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(rows[-1]["consecutive_failures"], 3)
        self.assertIn("invalid sync peer count", rows[-1]["error"])

    def test_a_native_hash_mismatch_is_a_failure(self):
        result, rows = self.run_monitor("wrong_hash")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(rows[-1]["consecutive_failures"], 3)
        self.assertIn("hash differs", rows[-1]["error"])


if __name__ == "__main__":
    unittest.main()
