#!/usr/bin/env python3
"""Exercise actual HAProxy admission/rates and the image's HTTP process pair."""

import concurrent.futures
import http.client
import http.server
import importlib.util
import ipaddress
import os
from pathlib import Path
import re
import resource
import socket
import subprocess
import tempfile
import threading
import time
import unittest

HERE = Path(__file__).resolve().parent
TEMPLATE = (Path("/etc/nginx/mmx/haproxy.cfg.template")
            if Path("/etc/nginx/mmx/haproxy.cfg.template").exists()
            else HERE / "haproxy.cfg.template")


def unused_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class Origin(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    disable_nagle_algorithm = True

    def do_GET(self):
        if self.path == "/blocked":
            self.server.release.wait(8)
        body = b"ok\n"
        try:
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_):
        pass


class Frontend:
    def __init__(self, trusted=False):
        self.temp = tempfile.TemporaryDirectory(prefix="mmx-apps-limits-")
        self.root = Path(self.temp.name)
        self.port = unused_port()
        self.origin = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Origin)
        self.origin.daemon_threads = True
        self.origin.release = threading.Event()
        self.thread = threading.Thread(target=self.origin.serve_forever, daemon=True)
        self.thread.start()
        self.trusted = trusted
        self.sockets = []
        (self.root / "trusted-proxies.lst").write_text("127.0.0.1/32\n" if trusted else "")
        text = TEMPLATE.read_text().replace("${PORT}", str(self.port))
        text = text.replace("${CLIENT_IP_HEADER}", "X-Forwarded-For")
        text = text.replace("/tmp/mmx-apps", str(self.root))
        text = text.replace("global\n", f"global\n    stats socket {self.root}/admin.sock level admin\n", 1)
        text = text.replace(f"server nginx {self.root}/nginx.sock",
                            f"server nginx 127.0.0.1:{self.origin.server_port}")
        # Source-address fixture: HAProxy decodes synthetic IPv4/IPv6 addresses
        # on this isolated loopback listener. No shipped listener accepts PROXY.
        if not trusted:
            text = text.replace(f"bind 0.0.0.0:{self.port}",
                                f"bind 127.0.0.1:{self.port} accept-proxy")
        self.config = self.root / "haproxy.cfg"
        self.config.write_text(text)
        self.log = (self.root / "log").open("w+")
        self.process = subprocess.Popen(["haproxy", "-db", "-f", str(self.config)],
                                        stdout=self.log, stderr=self.log)
        for _ in range(100):
            if self.process.poll() is not None:
                self.log.seek(0)
                raise AssertionError(self.log.read())
            try:
                with socket.create_connection(("127.0.0.1", self.port), 0.1):
                    break
            except OSError:
                time.sleep(0.02)
        self.wait_connections(0)

    def wait_connections(self, expected, table="total_clients"):
        last = ""
        for _ in range(300):
            with socket.socket(socket.AF_UNIX) as admin:
                admin.settimeout(1)
                admin.connect(str(self.root / "admin.sock"))
                admin.sendall(f"show table {table}\n".encode())
                admin.shutdown(socket.SHUT_WR)
                chunks = []
                while data := admin.recv(65536):
                    chunks.append(data)
            last = b"".join(chunks).decode()
            match = re.search(r"conn_cur=(\d+)", last)
            if (int(match[1]) if match else 0) == expected:
                return
            time.sleep(0.01)
        raise AssertionError(f"Expected {expected} connections: {last}")

    def close(self):
        self.origin.release.set()
        for connection in self.sockets:
            connection.close()
        self.process.terminate()
        self.process.wait(timeout=5)
        self.origin.shutdown()
        self.origin.server_close()
        self.thread.join()
        self.log.close()
        self.temp.cleanup()

    def connect(self, address="198.18.0.1", physical="127.0.0.1"):
        connection = socket.create_connection(("127.0.0.1", self.port), 2,
                                              source_address=(physical, 0))
        self.sockets.append(connection)
        if not self.trusted:
            v6 = ":" in address
            family, destination = ("TCP6", "::1") if v6 else ("TCP4", "127.0.0.1")
            connection.sendall(f"PROXY {family} {address} {destination} 12345 {self.port}\r\n".encode())
        return connection

    def get(self, address="198.18.0.1", host="wallet.mmx.network", headers="", path="/", physical="127.0.0.1"):
        connection = self.connect(address, physical)
        if self.trusted and "X-Forwarded-For:" not in headers:
            headers += f"X-Forwarded-For: {address}\r\n"
        connection.sendall(f"GET {path} HTTP/1.1\r\nHost: {host}\r\n{headers}\r\n".encode())
        response = http.client.HTTPResponse(connection)
        response.begin()
        assert response.getheader("Connection", "").lower() == "close"
        body = response.read()
        result = response.status, dict(response.getheaders()), body
        # Connection: close must describe actual transport behavior.
        self.assert_eof(connection)
        connection.close()
        return result

    @staticmethod
    def assert_eof(connection):
        connection.settimeout(2)
        assert connection.recv(1) == b"", "HTTP socket stayed open after its response"


class LimitsTest(unittest.TestCase):
    def front(self, trusted=False):
        front = Frontend(trusted)
        self.addCleanup(front.close)
        return front

    def assert_open(self, connection):
        connection.settimeout(0.05)
        with self.assertRaises(socket.timeout):
            connection.recv(1)

    def assert_rejected(self, connection):
        connection.settimeout(2)
        try:
            self.assertEqual(connection.recv(1), b"")
        except ConnectionResetError:
            pass

    def test_ipv4_and_ipv6_connection_budgets(self):
        front = self.front()
        for address in ["198.18.0.1", "2001:db8:1234:1::1"]:
            for _ in range(10):
                self.assert_open(front.connect(address))
            sibling = "2001:db8:1234:1::abcd" if ":" in address else address
            self.assert_rejected(front.connect(sibling))
        self.assert_open(front.connect("198.18.0.2"))
        # The /48 budget is shared by different /64s, on top of each /64 cap.
        for group in range(2, 11):
            for _ in range(10):
                front.connect(f"2001:db8:1234:{group:x}::1")
        # PROXY frames may complete on different threads in a different order
        # from TCP connects. Establish the full budget before probing overflow.
        front.wait_connections(100, "prefixes48")
        self.assert_rejected(front.connect("2001:db8:1234:ffff::1"))
        self.assert_open(front.connect("2001:db8:1235::1"))

    def test_total_budget_across_distinct_visitors(self):
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        resource.setrlimit(resource.RLIMIT_NOFILE, (min(hard, max(soft, 10000)), hard))
        front = self.front()
        for index in range(4096):
            front.connect(str(ipaddress.IPv4Address(int(ipaddress.IPv4Address("198.18.0.1")) + index)))
        front.wait_connections(4096)
        self.assert_rejected(front.connect("198.19.0.1"))

    def test_request_rates_share_hosts_and_reject_spoofed_headers(self):
        front = self.front()
        for index in range(100):
            self.assertEqual(front.get(host="wallet.mmx.network" if index % 2 else "mmx.network",
                                       headers=f"X-Forwarded-For: 203.0.113.{index + 1}\r\nX-Mmx-Client: 192.0.2.1\r\n")[0], 200)
        status, headers, _ = front.get(host="explore.mmx.network")
        self.assertEqual(status, 429)
        self.assertEqual(headers["retry-after"], "10")
        self.assertEqual(headers["cache-control"], "no-store")
        self.assertEqual(front.get("198.18.0.2")[0], 200)

    def test_ipv6_rate_prefixes(self):
        front = self.front()
        for index in range(100):
            self.assertEqual(front.get(f"2001:db8:1234:1::{index + 1:x}")[0], 200)
        self.assertEqual(front.get("2001:db8:1234:1::ffff")[0], 429)
        # Rejected /64 requests also count against /48. Start a fresh budget
        # to verify exactly 1,000 requests spread over ten distinct /64s.
        front = self.front()
        for group in range(1, 11):
            for _ in range(100):
                self.assertEqual(front.get(f"2001:db8:1234:{group:x}::1")[0], 200)
        self.assertEqual(front.get("2001:db8:1234:ffff::1")[0], 429)
        self.assertEqual(front.get("2001:db8:1235::1")[0], 200)

    def test_trusted_proxy_identity_and_connection_cap(self):
        front = self.front(trusted=True)
        for _ in range(10):
            connection = front.connect()
            connection.sendall(b"GET /blocked HTTP/1.1\r\nHost: wallet.mmx.network\r\nX-Forwarded-For: 2001:db8:1234:1::1\r\n\r\n")
            self.assert_open(connection)
        self.assertEqual(front.get("2001:db8:1234:1::2")[0], 429)
        self.assertEqual(front.get("2001:db8:1234:2::1")[0], 200)
        for value in ["bad", "198.18.0.1, 198.18.0.2"]:
            status, headers, _ = front.get(headers=f"X-Forwarded-For: {value}\r\n")
            self.assertEqual(status, 400)
            self.assertEqual(headers["cache-control"], "no-store")
        for index in range(100):
            self.assertEqual(front.get(f"2001:db8:1235:1::{index + 1:x}")[0], 200)
        self.assertEqual(front.get("2001:db8:1235:1::ffff")[0], 429)
        self.assertEqual(front.get("2001:db8:1235:2::1")[0], 200)
        # A different physical peer must not gain access to forwarded identity.
        for index in range(100):
            self.assertEqual(front.get(f"198.19.0.{index + 1}", physical="127.0.0.2")[0], 200)
        self.assertEqual(front.get("198.19.1.1", physical="127.0.0.2")[0], 429)

    def test_complete_and_oversize_bodies(self):
        front = self.front()
        for chunked in (False, True):
            for size, expected in ((1024, 200), (1025, 413)):
                connection = front.connect()
                framing = "Transfer-Encoding: chunked" if chunked else f"Content-Length: {size}"
                body = b"x" * size
                if chunked:
                    body = f"{size:x}\r\n".encode() + body + b"\r\n0\r\n\r\n"
                connection.sendall(f"GET / HTTP/1.1\r\nHost: wallet.mmx.network\r\n{framing}\r\n\r\n".encode() + body)
                response = http.client.HTTPResponse(connection)
                response.begin()
                self.assertEqual(response.status, expected)
                if expected == 413:
                    self.assertEqual(response.getheader("Cache-Control"), "no-store")
                response.read()
                front.assert_eof(connection)
                connection.close()

    def test_idle_and_body_deadlines(self):
        front = self.front()
        def idle():
            connection = front.connect("198.18.1.1")
            start = time.monotonic()
            connection.settimeout(18)
            while connection.recv(4096):
                pass
            return time.monotonic() - start

        def upload(chunked):
            connection = front.connect("198.18.1.2" if chunked else "198.18.1.3")
            framing = "Transfer-Encoding: chunked" if chunked else "Content-Length: 1024"
            connection.sendall(f"GET / HTTP/1.1\r\nHost: wallet.mmx.network\r\nX-Padding: {'x' * 10000}\r\n{framing}\r\n\r\n".encode())
            if chunked:
                connection.sendall(b"400\r\n")
            connection.settimeout(0.1)
            start = time.monotonic()
            response = b""
            while time.monotonic() - start < 7:
                try:
                    connection.sendall(b"x")
                    response = connection.recv(4096)
                    if response:
                        break
                except socket.timeout:
                    pass
                time.sleep(0.1)
            self.assertIn(b" 408 ", response)
            return time.monotonic() - start

        def headers():
            connection = front.connect("198.18.1.4")
            connection.sendall(b"GET / HTTP/1.1\r\nHost: ")
            connection.settimeout(7)
            start = time.monotonic()
            self.assertIn(b" 408 ", connection.recv(4096))
            return time.monotonic() - start

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            pending = [pool.submit(idle), pool.submit(upload, False), pool.submit(upload, True), pool.submit(headers)]
            elapsed = [task.result() for task in pending]
        self.assertTrue(14 <= elapsed[0] <= 17, elapsed)
        self.assertTrue(all(4.5 <= value <= 6.5 for value in elapsed[1:]), elapsed)


@unittest.skipUnless(Path("/usr/local/bin/mmx-apps-entrypoint").exists(), "requires the image runtime")
class ImageRuntimeTest(unittest.TestCase):
    def test_sites_and_supervision(self):
        spec = importlib.util.spec_from_file_location("image_test", HERE / "test-image.py")
        image_test = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(image_test)
        port = unused_port()
        with tempfile.TemporaryFile(mode="w+") as log:
            process = subprocess.Popen(["/sbin/tini", "-g", "--", "/usr/local/bin/mmx-apps-entrypoint",
                                        "nginx", "-e", "stderr", "-c", "/tmp/mmx-apps/nginx.conf", "-g", "daemon off;"],
                                       env=dict(os.environ, PORT=str(port)), stdout=log, stderr=log)
            try:
                for _ in range(100):
                    try:
                        if image_test.request(port, "localhost", "/healthz")[0] == 200:
                            break
                    except OSError:
                        pass
                    if process.poll() is not None:
                        log.seek(0)
                        self.fail(log.read())
                    time.sleep(0.05)
                else:
                    self.fail("image HTTP listener did not start")
                for host, wallet, website in [("wallet.mmx.network", True, False),
                                               ("explore.mmx.network", False, False),
                                               ("mmx.network", False, True)]:
                    image_test.check_site(port, host, "https://rpc.mmx.network", wallet, website)
                # Stopping one component must terminate the other and the image
                # supervisor. Match only this fixture's exact nginx PID file.
                pid = int(Path("/tmp/mmx-apps/nginx.pid").read_text())
                self.assertEqual(Path(f"/proc/{pid}/exe").resolve().name, "nginx")
                self.assertIn(b"/tmp/mmx-apps/nginx.conf", Path(f"/proc/{pid}/cmdline").read_bytes())
                parent = int(re.search(r"^PPid:\s+(\d+)", Path(f"/proc/{pid}/status").read_text(), re.MULTILINE)[1])
                self.assertEqual(int(re.search(r"^PPid:\s+(\d+)", Path(f"/proc/{parent}/status").read_text(), re.MULTILINE)[1]), process.pid)
                os.kill(pid, 15)
                process.wait(timeout=5)
                with self.assertRaises(OSError):
                    socket.create_connection(("127.0.0.1", port), 0.2)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
