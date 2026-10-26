"""HTTP transport for the desktop command checks, with no command replay."""
import http.client
import json


class WebDriverClient:
    def __init__(self, host, port, timeout=120):
        self.connection = http.client.HTTPConnection(host, port, timeout=timeout)

    def close(self):
        self.connection.close()

    def request(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        try:
            # urllib forces Connection: close on every request. tauri-driver
            # forwards that header to its pooled WebKit connection, which can
            # then reset between commands. Keep one HTTP/1.1 connection instead.
            self.connection.request(method, path, body=data,
                                    headers={"Content-Type": "application/json"})
            response = self.connection.getresponse()
            payload = response.read()
        except (OSError, http.client.HTTPException):
            # Reset HTTPConnection's state so the existing startup loop can
            # retry connection refusal. Never replay a command: its side effects
            # may have happened even if the response was lost.
            self.close()
            raise
        if response.status >= 400:
            raise RuntimeError(f"WebDriver {method} {path}: {payload[:300]!r}")
        return json.loads(payload).get("value")
