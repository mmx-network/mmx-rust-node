#!/usr/bin/env python3
"""Exercise the real HTTP image, using only loopback and disposable containers."""

import argparse
import gzip
import http.client
import json
import re
import shlex
import subprocess
import time
import uuid
from contextlib import contextmanager
from html.parser import HTMLParser
from urllib.robotparser import RobotFileParser
from urllib.parse import urlsplit
from xml.etree import ElementTree


def docker(*args, check=True):
    return subprocess.run(["docker", *args], check=check, capture_output=True, text=True)


@contextmanager
def running_image(image, image_id, port=8080, wallet="wallet.mmx.network", explorer="explore.mmx.network", website="mmx.network", website_www="www.mmx.network", overrides=False, root_console=False):
    token = str(uuid.uuid4())
    args = [
        "run", "--detach", "--read-only", "--tmpfs", "/tmp:rw,noexec,nosuid,size=16m",
        "--cpus", "2", "--memory", "128m", "--cap-drop", "ALL",
        "--security-opt", "no-new-privileges:true", "--label", f"mmx-apps-test={token}",
        "--publish", f"127.0.0.1::{port}",
    ]
    if overrides:
        args += ["--env", f"PORT={port}", "--env", f"WALLET_HOST={wallet}", "--env", f"EXPLORER_HOST={explorer}",
                 "--env", f"WEBSITE_HOST={website}", "--env", f"WEBSITE_WWW_HOST={website_www}"]
    if root_console:
        # VM supervisors can open console pipes as root before dropping UID.
        # Keep those inherited descriptors, then exec the image's actual command
        # as nginx. Reopening /dev/stdout or /dev/stderr must not be necessary.
        config = json.loads(docker("image", "inspect", "--format", "{{json .Config}}", image).stdout)
        command = shlex.join(config["Entrypoint"] + config["Cmd"])
        args += ["--user", "0:0", "--cap-add", "SETUID", "--cap-add", "SETGID", "--entrypoint", "/bin/sh"]
        container = docker(*args, image, "-c", f"exec su -p -s /bin/sh nginx -c {shlex.quote(command)}").stdout.strip()
    else:
        container = docker(*args, image).stdout.strip()
    try:
        inspect = json.loads(docker("inspect", container).stdout)[0]
        bindings = inspect["NetworkSettings"].get("Ports", {}).get(f"{port}/tcp")
        if not bindings:
            logs = docker("logs", container)
            raise AssertionError(logs.stdout + logs.stderr or str(inspect["State"]))
        binding = bindings[0]
        host_port = int(binding["HostPort"])
        for attempt in range(100):
            try:
                if request(host_port, "localhost", "/healthz")[0] == 200:
                    break
            except (ConnectionError, OSError, http.client.HTTPException):
                pass
            time.sleep(0.1)
        else:
            raise AssertionError(docker("logs", container).stdout + docker("logs", container).stderr)
        yield container, host_port, wallet, explorer, website, website_www
    finally:
        # Verify the entire owned container identity before stopping anything.
        inspect = json.loads(docker("inspect", container).stdout)[0]
        assert inspect["Id"] == container and inspect["Image"] == image_id
        assert inspect["Config"]["Labels"].get("mmx-apps-test") == token
        docker("stop", "--time", "5", container)
        docker("rm", container)


def request(port, host, path="/", method="GET", headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
    try:
        connection.request(method, path, headers={"Host": f"{host}:{port}", **(headers or {})})
        response = connection.getresponse()
        return response.status, {key.lower(): value for key, value in response.getheaders()}, response.read()
    finally:
        connection.close()


def security(headers, rpc_origin, wallet, website=False):
    policy = headers["content-security-policy"]
    if website:
        assert rpc_origin not in policy, policy
        assert "connect-src 'none'" in policy, policy
    else:
        assert f"connect-src 'self' {rpc_origin}" in policy, policy
    assert "frame-ancestors 'none'" in policy
    assert ("'wasm-unsafe-eval'" in policy) == wallet
    assert headers["x-content-type-options"] == "nosniff"
    assert headers["x-frame-options"] == "DENY"
    assert headers["referrer-policy"] == "no-referrer"
    assert "set-cookie" not in headers
    if wallet:
        assert headers["cross-origin-opener-policy"] == "same-origin"
        assert headers["cross-origin-resource-policy"] == "same-origin"
        assert "clipboard-read=(self)" in headers["permissions-policy"]
        assert "max-age=" in headers["strict-transport-security"]


class SearchMetadata(HTMLParser):
    def __init__(self, html):
        super().__init__()
        self.meta = {}
        self.canonicals = []
        self.site_urls = []
        self.website_scopes = 0
        self.feed(html)

    def handle_starttag(self, tag, attributes):
        attrs = dict(attributes)
        if "itemscope" in attrs and attrs.get("itemtype") == "https://schema.org/WebSite":
            self.website_scopes += 1
        if tag == "meta":
            for key in ["name", "property", "itemprop"]:
                if key in attrs:
                    self.meta[(key, attrs[key])] = attrs.get("content", "")
        if tag == "link":
            if attrs.get("rel") == "canonical":
                self.canonicals.append(attrs.get("href"))
            if attrs.get("itemprop") == "url":
                self.site_urls.append(attrs.get("href"))


def check_search(port, host, html, rpc_origin, wallet, website):
    public_host = "mmx.network" if website else "wallet.mmx.network" if wallet else "explore.mmx.network"
    name = "MMX Network" if website else "MMX Wallet" if wallet else "MMX Explorer"
    canonical = f"https://{public_host}/"
    metadata = SearchMetadata(html.decode())
    assert metadata.canonicals == [canonical], metadata.canonicals
    assert metadata.website_scopes == 1
    assert metadata.site_urls == [canonical]
    assert metadata.meta[("itemprop", "name")] == name
    assert len(metadata.meta[("name", "description")]) > 80
    directives = {value.strip() for value in metadata.meta[("name", "robots")].split(",")}
    assert {"index", "follow", "max-image-preview:large"} <= directives
    assert not {"noindex", "nofollow", "none"} & directives
    assert metadata.meta[("property", "og:url")] == canonical
    assert metadata.meta[("property", "og:site_name")] == name
    assert name in metadata.meta[("property", "og:title")]
    assert metadata.meta[("property", "og:description")] == metadata.meta[("name", "description")]
    assert metadata.meta[("property", "og:image")].startswith("https://")
    assert metadata.meta[("name", "twitter:card")] in ["summary", "summary_large_image"]
    # Microdata supplies the site name without weakening the no-inline-script CSP.
    assert not re.search(rb'<script\b(?![^>]*\bsrc=)[^>]*>', html)

    status, headers, robots = request(port, host, "/robots.txt")
    assert status == 200 and headers["content-type"].startswith("text/plain")
    assert headers["cache-control"] == "no-cache"
    security(headers, rpc_origin, wallet, website)
    rules = RobotFileParser()
    rules.parse(robots.decode().splitlines())
    assert rules.site_maps() == [f"{canonical}sitemap.xml"]
    for agent in ["Googlebot", "bingbot", "ExampleBot"]:
        assert rules.can_fetch(agent, canonical)
        assert rules.can_fetch(agent, f"{canonical}assets/example.js")
        assert not rules.can_fetch(agent, f"{canonical}healthz")

    status, headers, sitemap = request(port, host, "/sitemap.xml")
    assert status == 200 and "xml" in headers["content-type"]
    assert headers["cache-control"] == "no-cache"
    security(headers, rpc_origin, wallet, website)
    root = ElementTree.fromstring(sitemap)
    namespace = "{http://www.sitemaps.org/schemas/sitemap/0.9}"
    assert root.tag == f"{namespace}urlset"
    assert [loc.text for loc in root.findall(f"{namespace}url/{namespace}loc")] == [canonical]


def check_site(port, host, rpc_origin, wallet=False, website=False):
    status, headers, html = request(port, host)
    assert status == 200, (host, status)
    title = b"<title>MMX" if website else b"<title>MMX Wallet</title>" if wallet else b"<title>MMX Explorer"
    assert title in html
    check_search(port, host, html, rpc_origin, wallet, website)
    if website:
        assert b"https://wallet.mmx.network" in html
        assert b'<link rel="canonical" href="https://mmx.network/">' in html
        assert re.search(rb'<meta name="description" content="[^"]+">', html)
        for path, content_type in [
            ("/site.webmanifest", "application/manifest+json"), ("/share.png", "image/png"),
        ]:
            status, public_headers, body = request(port, host, path)
            assert status == 200 and body
            assert content_type in public_headers["content-type"]
            assert public_headers["cache-control"] == "no-cache"
            security(public_headers, rpc_origin, wallet, website)
    assert headers["cache-control"] == "no-cache"
    security(headers, rpc_origin, wallet, website)
    status, headers, _ = request(port, host, "/index.html")
    assert status == 200 and headers["cache-control"] == "no-cache"
    security(headers, rpc_origin, wallet, website)

    # The same file supports conditional revalidation and precompressed delivery.
    status, headers, body = request(port, host, headers={"Accept-Encoding": "gzip"})
    assert status == 200 and headers["content-encoding"] == "gzip"
    assert "accept-encoding" in headers["vary"].lower()
    assert gzip.decompress(body) == html
    status, headers, body = request(port, host, headers={"If-None-Match": headers["etag"], "Accept-Encoding": "gzip"})
    assert status == 304 and body == b"" and headers["cache-control"] == "no-cache"
    security(headers, rpc_origin, wallet, website)

    assets = re.findall(r'(?:src|href)="(?:\./|/)(assets/[^"?]+\.(?:js|css))"', html.decode())
    assert len(assets) >= 2, assets
    for asset in assets:
        assert re.search(r"-[A-Za-z0-9_-]+\.(?:js|css)$", asset), asset
        path = "/" + asset
        status, headers, raw = request(port, host, path)
        assert status == 200 and raw
        assert headers["cache-control"] == "public, max-age=31536000, immutable"
        assert ("javascript" if path.endswith(".js") else "text/css") in headers["content-type"]
        security(headers, rpc_origin, wallet, website)
        status, headers, compressed = request(port, host, path, headers={"Accept-Encoding": "gzip"})
        assert status == 200 and headers["content-encoding"] == "gzip"
        assert gzip.decompress(compressed) == raw
        security(headers, rpc_origin, wallet, website)
        status, headers, body = request(port, host, path, method="HEAD")
        assert status == 200 and not body and int(headers["content-length"]) == len(raw)
        status, headers, body = request(port, host, path, headers={"If-None-Match": headers["etag"]})
        assert status == 304 and not body
        assert headers["cache-control"] == "public, max-age=31536000, immutable"
        security(headers, rpc_origin, wallet, website)

    for path in ["/assets/missing.js", "/missing", "/.env", "/_headers"]:
        status, headers, _ = request(port, host, path)
        assert status == 404 and headers["cache-control"] == "no-store", (path, status, headers)
        security(headers, rpc_origin, wallet, website)
    status, headers, _ = request(port, host, method="POST")
    assert status == 405 and headers["cache-control"] == "no-store"
    security(headers, rpc_origin, wallet, website)
    status, headers, _ = request(port, host, "/favicon.svg")
    assert status == 200 and headers["cache-control"] == "no-cache"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", nargs="?", default="mmx-apps")
    parser.add_argument("--rpc-origin", default="https://rpc.mmx.network")
    args = parser.parse_args()
    rpc_origin = args.rpc_origin.rstrip("/")
    assert urlsplit(rpc_origin).scheme in ["http", "https"]
    image_id = docker("image", "inspect", "--format", "{{.Id}}", args.image).stdout.strip()

    with running_image(args.image, image_id, 18080, "wallet.test", "explore.test", "website.test", "www.website.test", overrides=True) as (container, port, wallet, explorer, website, website_www):
        assert docker("exec", container, "id", "-u").stdout.strip() != "0"
        check_site(port, wallet, rpc_origin, wallet=True)
        check_site(port, explorer, rpc_origin, wallet=False)
        check_site(port, website, rpc_origin, website=True)
        assert request(port, website_www)[2] == request(port, website)[2]
        # Runtime hostname changes move the complete sites without weakening
        # their policies or leaking another site's files into a host.
        assert request(port, "mmx.network")[0] == 404
        assert request(port, website)[2] != request(port, wallet)[2]
        assert request(port, website)[2] != request(port, explorer)[2]
        status, headers, _ = request(port, "unconfigured.test")
        assert status == 404 and headers["cache-control"] == "no-store"
        wasm = docker("exec", container, "find", "/srv/mmx/wallet/assets", "-maxdepth", "1", "-name", "*.wasm").stdout.splitlines()
        assert len(wasm) == 1, wasm
        path = wasm[0].removeprefix("/srv/mmx/wallet")
        assert request(port, website, path)[0] == 404
        assert request(port, explorer, path)[0] == 404
        status, headers, raw = request(port, wallet, path)
        assert status == 200 and headers["content-type"] == "application/wasm" and raw[:4] == b"\x00asm"
        assert headers["cache-control"] == "public, max-age=31536000, immutable"
        status, headers, body = request(port, wallet, path, headers={"Accept-Encoding": "gzip"})
        assert status == 200 and headers["content-encoding"] == "gzip" and gzip.decompress(body) == raw
        status, headers, body = request(port, wallet, path, headers={"Range": "bytes=0-15"})
        assert status == 206 and body == raw[:16]
        assert headers["cache-control"] == "public, max-age=31536000, immutable"
        status, headers, _ = request(port, wallet, "/manifest.webmanifest")
        assert status == 200 and headers["content-type"].startswith("application/manifest+json")
        assert headers["cache-control"] == "no-cache"
        docker("exec", container, "wget", "-q", "-O", "/dev/null", "http://127.0.0.1:18080/healthz")

    with running_image(args.image, image_id) as (_, port, wallet, explorer, website, website_www):
        for host, title in [(wallet, b"MMX Wallet"), (explorer, b"MMX Explorer"), (website, b"<title>MMX"), (website_www, b"<title>MMX")]:
            status, _, body = request(port, host)
            assert status == 200 and title in body

    with running_image(args.image, image_id, root_console=True) as (container, port, wallet, explorer, website, website_www):
        status = docker("exec", "--user", "nginx", container, "cat", "/proc/1/status").stdout
        uid = docker("exec", "--user", "nginx", container, "id", "-u").stdout.strip()
        assert uid != "0" and re.search(rf"^Uid:\s+{uid}\s+{uid}\s+{uid}\s+{uid}$", status, re.MULTILINE)
        # Verify the fixture really forbids reopening its supervisor's pipes.
        for descriptor in ["1", "2"]:
            result = docker("exec", "--user", "nginx", container, "test", "-w", f"/proc/1/fd/{descriptor}", check=False)
            assert result.returncode != 0, f"Console FD {descriptor} is unexpectedly reopenable"
        check_site(port, wallet, rpc_origin, wallet=True)
        check_site(port, explorer, rpc_origin, wallet=False)
        check_site(port, website, rpc_origin, website=True)
        logs = docker("logs", container)
        assert '"GET / HTTP/1.1" 200' in logs.stderr
        assert '"GET /missing HTTP/1.1" 404' in logs.stderr
        assert "Permission denied" not in logs.stderr and "could not open error log" not in logs.stderr

    for settings in [
        ["PORT=0"], ["PORT=65536"], ["PORT=80;bad"], ["WALLET_HOST=bad;host"],
        ["WEBSITE_HOST=bad;host"], ["WEBSITE_WWW_HOST=bad;host"],
        ["WALLET_HOST=EXPLORE.MMX.NETWORK"], ["WEBSITE_HOST=WALLET.MMX.NETWORK"],
        ["WEBSITE_WWW_HOST=EXPLORE.MMX.NETWORK"], ["WEBSITE_WWW_HOST=MMX.NETWORK"],
    ]:
        env = [part for setting in settings for part in ["--env", setting]]
        result = docker("run", "--rm", "--network", "none", *env, args.image, "true", check=False)
        assert result.returncode != 0, settings
    print(f"{args.image}: website/wallet/explorer routing and isolation, search metadata/robots/sitemaps, port/host overrides, CSP, caching, gzip, WASM, read-only/non-root runtime, root-owned console and invalid settings passed")


if __name__ == "__main__":
    main()
