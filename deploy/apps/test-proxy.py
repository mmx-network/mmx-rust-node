#!/usr/bin/env python3
"""Test TLS Caddy -> the shipped entrypoint -> HAProxy -> nginx under VM limits."""

import concurrent.futures
import http.client
import importlib.util
import os
from pathlib import Path
import re
import resource
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import unittest

from h2.connection import H2Connection
from h2.config import H2Configuration
from h2.events import DataReceived, ResponseReceived, StreamEnded, StreamReset, ConnectionTerminated


HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("image_test", HERE / "test-image.py")
image_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(image_test)


def unused_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def vm_limits():
    # These are the actual limits that caused the 2026-11-12 production outage.
    resource.setrlimit(resource.RLIMIT_NOFILE, (1024, 4096))
    os.sched_setaffinity(0, {min(os.sched_getaffinity(0))})


class LoopbackTLS(http.client.HTTPSConnection):
    def connect(self):
        raw = socket.create_connection(("127.0.0.1", self.port), self.timeout, self.source_address)
        self.sock = self._context.wrap_socket(raw, server_hostname=self.host)


class HTTP2Client:
    def __init__(self, host, port, context, source):
        context.set_alpn_protocols(["h2"])
        raw = socket.create_connection(("127.0.0.1", port), 10, (source, 0))
        self.socket = context.wrap_socket(raw, server_hostname=host)
        assert self.socket.selected_alpn_protocol() == "h2"
        self.host = host
        self.protocol = H2Connection(config=H2Configuration(header_encoding="utf-8"))
        self.protocol.initiate_connection()
        self.socket.sendall(self.protocol.data_to_send())

    def request(self, paths):
        replies, ended = {}, set()
        for path in paths:
            stream = self.protocol.get_next_available_stream_id()
            replies[stream] = [{}, bytearray()]
            self.protocol.send_headers(stream, [(":method", "GET"), (":scheme", "https"),
                                                (":authority", self.host), (":path", path)], end_stream=True)
        self.socket.sendall(self.protocol.data_to_send())
        while len(ended) < len(replies):
            data = self.socket.recv(65536)
            assert data, "HTTP/2 connection closed before completing its streams"
            for event in self.protocol.receive_data(data):
                if isinstance(event, ResponseReceived):
                    replies[event.stream_id][0].update(event.headers)
                elif isinstance(event, DataReceived):
                    replies[event.stream_id][1].extend(event.data)
                    self.protocol.acknowledge_received_data(event.flow_controlled_length, event.stream_id)
                elif isinstance(event, StreamEnded):
                    ended.add(event.stream_id)
                elif isinstance(event, (StreamReset, ConnectionTerminated)):
                    raise AssertionError(f"HTTP/2 request failed: {event}")
            pending = self.protocol.data_to_send()
            if pending:
                self.socket.sendall(pending)
        return [(int(headers[":status"]), headers, bytes(body)) for headers, body in replies.values()]

    def close(self):
        self.socket.close()


class ProxyTest(unittest.TestCase):
    def test_single_cpu_vm_through_tls_proxy(self):
        origin_port, tls_port = unused_port(), unused_port()
        hosts = ["wallet.mmx.network", "explore.mmx.network", "mmx.network", "www.mmx.network"]
        with tempfile.TemporaryDirectory(prefix="mmx-apps-proxy-") as directory:
            root = Path(directory)
            config = root / "Caddyfile"
            config.write_text("{\n admin off\n persist_config off\n skip_install_trust\n"
                              " auto_https disable_redirects\n}\n" +
                              ", ".join(f"https://{host}:{tls_port}" for host in hosts) + " {\n"
                              " bind 127.0.0.1\n tls internal\n header X-Mmx-Test-Proxy caddy\n"
                              f" reverse_proxy 127.0.0.1:{origin_port}\n}}\n")
            with (root / "origin.log").open("w+") as origin_log, (root / "proxy.log").open("w+") as proxy_log:
                origin = subprocess.Popen(
                    ["/sbin/tini", "-g", "--", "/usr/local/bin/mmx-apps-entrypoint", "nginx", "-e", "stderr",
                     "-c", "/tmp/mmx-apps/nginx.conf", "-g", "daemon off;"],
                    env=dict(os.environ, PORT=str(origin_port), TRUSTED_PROXY_CIDRS="127.0.0.1/32"),
                    stdout=origin_log, stderr=origin_log, preexec_fn=vm_limits)
                proxy = None
                original_request = image_test.request_once
                try:
                    for _ in range(100):
                        if origin.poll() is not None:
                            self.fail("image exited under VM resource limits")
                        try:
                            if image_test.request_once(origin_port, hosts[0], "/healthz",
                                                       headers={"X-Forwarded-For": "127.0.0.1"})[0] == 200:
                                break
                        except (OSError, http.client.HTTPException):
                            pass
                        time.sleep(0.05)
                    else:
                        origin_log.seek(0)
                        self.fail("image did not become ready under VM resource limits:\n" + origin_log.read())

                    proxy = subprocess.Popen(["caddy", "run", "--config", str(config), "--adapter", "caddyfile"],
                                             env=dict(os.environ, XDG_DATA_HOME=str(root / "data"),
                                                      XDG_CONFIG_HOME=str(root / "config")),
                                             stdout=proxy_log, stderr=proxy_log)
                    ca = root / "data/caddy/pki/authorities/local/root.crt"
                    for _ in range(100):
                        if ca.exists():
                            break
                        if proxy.poll() is not None:
                            self.fail("TLS proxy exited")
                        time.sleep(0.05)
                    self.assertTrue(ca.exists(), "TLS proxy did not issue its test CA")
                    context = ssl.create_default_context(cafile=str(ca))

                    def request(_, host, path="/", method="GET", headers=None, source="127.0.0.1"):
                        connection = LoopbackTLS(host, tls_port, timeout=5, context=context,
                                                 source_address=(source, 0))
                        try:
                            connection.request(method, path, headers={"Host": host, **(headers or {})})
                            response = connection.getresponse()
                            result = response.status, dict((key.lower(), value) for key, value in response.getheaders()), response.read()
                            self.assertEqual(result[1].get("x-mmx-test-proxy"), "caddy")
                            return result
                        finally:
                            connection.close()

                    # Reuse all shipped content/security/cache/asset checks, but
                    # send every request through authenticated TLS and Caddy.
                    image_test.request_once = request
                    for host, wallet, website in [(hosts[0], True, False), (hosts[1], False, False), (hosts[2], False, True)]:
                        image_test.check_site(tls_port, host, "https://rpc.mmx.network", wallet, website)
                    self.assertEqual(image_test.request(tls_port, hosts[2])[2], image_test.request(tls_port, hosts[3])[2])

                    # Fetch a page's module and stylesheet concurrently, as a
                    # browser does, without silently retrying gateway errors.
                    html = image_test.request(tls_port, hosts[0])[2]
                    paths = re.findall(rb'(?:src|href)="\.?(/assets/[^\"]+\.(?:js|css))"', html)
                    self.assertGreaterEqual(len(paths), 2)
                    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                        results = list(pool.map(lambda path: request(tls_port, hosts[0], path.decode(), source="127.0.0.2"), paths))
                    self.assertTrue(all(status == 200 and body for status, _, body in results))
                    wasm = list(Path("/srv/mmx/wallet/assets").glob("*.wasm"))
                    self.assertEqual(len(wasm), 1)
                    status, headers, body = image_test.request(tls_port, hosts[0], "/assets/" + wasm[0].name)
                    self.assertEqual(status, 200)
                    self.assertEqual(headers["content-type"], "application/wasm")
                    self.assertEqual(body, wasm[0].read_bytes())
                    # The deployed HTTP proxy permits 50 concurrent visitors.
                    # Real distinct loopback clients exercise Caddy's overwrite
                    # of forwarded identities and the trusted-proxy counters.
                    with concurrent.futures.ThreadPoolExecutor(max_workers=50) as pool:
                        results = list(pool.map(lambda index: request(tls_port, hosts[0], "/healthz",
                                                                     source=f"127.0.0.{index}"), range(10, 60)))
                    self.assertTrue(all(status == 200 and body == b"ok\n" for status, _, body in results))
                    # HTTP/2 multiplexes independent requests on one TLS socket;
                    # each still traverses the HTTP/1 origin admission path.
                    client = HTTP2Client(hosts[0], tls_port, context, "127.0.0.3")
                    try:
                        results = client.request(["/", *[path.decode() for path in paths], "/assets/" + wasm[0].name])
                        self.assertTrue(all(status == 200 and body for status, _, body in results))
                        self.assertEqual(results[0][2], html)
                        self.assertEqual(results[-1][2], wasm[0].read_bytes())
                    finally:
                        client.close()
                    client = HTTP2Client(hosts[0], tls_port, context, "127.0.0.4")
                    try:
                        for _ in range(100):
                            self.assertEqual(client.request(["/healthz"])[0][0], 200)
                        status, headers, _ = client.request(["/healthz"])[0]
                        self.assertEqual(status, 429)
                        self.assertEqual(headers["retry-after"], "10")
                        self.assertEqual(headers["cache-control"], "no-store")
                    finally:
                        client.close()
                    self.assertIsNone(origin.poll())
                    self.assertIsNone(proxy.poll())
                finally:
                    image_test.request_once = original_request
                    # Only stop Popen-owned fixture processes, never host services.
                    for process in (proxy, origin):
                        if process is not None and process.poll() is None:
                            process.terminate()
                            process.wait(timeout=5)
                    origin_log.seek(0)
                    proxy_log.seek(0)
                    origin_output = origin_log.read()
                    proxy_output = proxy_log.read()
                    if sys.exc_info()[0] is not None:
                        print("Origin log:\n" + origin_output + "\nProxy log:\n" + proxy_output)
                self.assertNotIn("Cannot raise FD limit", origin_output)
                self.assertNotIn("exceed open file resource limit", origin_output)
                self.assertNotIn("compete for the same CPU", origin_output)


if __name__ == "__main__":
    unittest.main()
