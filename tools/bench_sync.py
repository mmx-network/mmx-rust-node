#!/usr/bin/env python3
"""Compare two node binaries replaying the same archived mainnet range.

Uses SQLite backups and isolated copies; the source node may remain running.
Every receiver downloads blocks over native peer sockets, re-verifies work and
executes the ledger. Both runs must reach the same source hash.
"""
import argparse
import contextlib
import json
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def snapshot(source, destination):
    destination.mkdir(parents=True)
    # Each peer and receiver generates its own authenticated node identity.
    # Archive admission precedes selected publication and never deletes blocks.
    # Snapshot selected first so the subsequent archive includes its entire tip.
    for name in ("selected", "archive"):
        directory = destination / "chain" / name
        directory.mkdir(parents=True)
        with contextlib.closing(sqlite3.connect(
            (source / "chain" / name / "state.sqlite").as_uri() + "?mode=ro",
            uri=True,
        )) as src, contextlib.closing(sqlite3.connect(directory / "state.sqlite")) as dst:
            src.backup(dst)


def starting_prefix(data, height):
    """Trim an isolated benchmark snapshot to a complete validated prefix.

    This uses the current Rust SQLite layout and never alters the source.
    Production rollback retains its entire archive.
    """
    with sqlite3.connect(data / "chain" / "selected" / "state.sqlite") as conn:
        conn.execute("DELETE FROM history WHERE v>=?", (height + 1,))
        conn.execute("UPDATE meta SET version=?,generation=generation+1 WHERE id=0",
                     (height + 1,))
    with sqlite3.connect(data / "chain" / "archive" / "state.sqlite") as conn:
        conn.execute("CREATE TEMP TABLE discarded(hash BLOB PRIMARY KEY) WITHOUT ROWID")
        conn.execute("INSERT OR IGNORE INTO discarded SELECT value FROM history "
                     "WHERE t='fork_order' AND substr(k,1,4)>? AND value IS NOT NULL",
                     (height.to_bytes(4, "big"),))
        conn.execute("DELETE FROM history WHERE t IN "
                     "('fork_blocks','fork_headers','fork_work','fork_invalid') "
                     "AND k IN (SELECT hash FROM discarded)")
        conn.execute("DELETE FROM history WHERE t='fork_order' AND substr(k,1,4)>?",
                     (height.to_bytes(4, "big"),))
        conn.execute("UPDATE meta SET generation=generation+1 WHERE id=0")


def compact(data):
    # Compact the offline base once; all five isolated copies then contain
    # only the benchmark range, rather than copying freed mainnet pages.
    for name in ("selected", "archive"):
        with contextlib.closing(sqlite3.connect(data / "chain" / name / "state.sqlite")) as conn:
            conn.execute("VACUUM")
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")


@contextlib.contextmanager
def launch(binary, root, data, log, *extra):
    with log.open("w") as output:
        process = subprocess.Popen([
            str(binary), "--mmx-root", str(root), "--data-dir", str(data),
            "--p2p-listen", "127.0.0.1:0", "--rpc-listen", "127.0.0.1:0",
            "--http-listen", "127.0.0.1:0", *extra,
        ], stdout=output, stderr=subprocess.STDOUT)
        try:
            yield process
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def endpoints(process, log):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        text = log.read_text()
        match = re.search(r"P2P ([^,]+), .*HTTP http://([^/]+)/rpc", text)
        if match:
            return match[1], "http://" + match[2] + "/rpc"
        if process.poll() is not None:
            raise RuntimeError(text)
        time.sleep(0.1)
    raise TimeoutError("node did not open its listeners")


def status(url):
    request = urllib.request.Request(
        url, data=b'{"jsonrpc":"2.0","id":1,"method":"get_status"}',
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)["result"]


def selected_hash(data):
    with contextlib.closing(sqlite3.connect(
        (data / "chain" / "selected" / "state.sqlite").as_uri() + "?mode=ro",
        uri=True,
    )) as conn:
        row = conn.execute(
            "SELECT value FROM history WHERE t='chain_meta' AND k=? "
            "ORDER BY v DESC LIMIT 1", (b"tip",),
        ).fetchone()
        return row[0].hex().upper()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--mirror-binary", type=Path,
                        help="defaults to examples/serve_snapshot beside candidate")
    parser.add_argument("--mmx-root", type=Path, default=Path("../mmx-node"))
    parser.add_argument("--from-height", type=int, required=True)
    parser.add_argument("--to-height", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 0 <= args.from_height < args.to_height:
        parser.error("require 0 <= from-height < to-height")
    args.output.mkdir(parents=True, exist_ok=True)
    args.output = args.output.resolve()
    with tempfile.TemporaryDirectory(prefix="sync-bench-", dir=args.output) as temporary:
        directory = Path(temporary)
        mirror_binary = (args.mirror_binary or args.candidate.parent / "examples" / "serve_snapshot").resolve()
        if not mirror_binary.is_file():
            parser.error("build the snapshot peer: cargo build --release -p mmx-node --example serve_snapshot")
        base = directory / "base"
        snapshot(args.source_dir.resolve(), base)
        starting_prefix(base, args.to_height)
        compact(base)
        with contextlib.ExitStack() as stack:
            peers = []
            expected = None
            for index in range(3):
                mirror = directory / f"mirror-{index}"
                snapshot(base, mirror)
                mirror_log = args.output / f"mirror-{index}.log"
                serving = stack.enter_context(launch(
                    mirror_binary, args.mmx_root.resolve(), mirror, mirror_log,
                    "--rollback-to", str(args.to_height)))
                peer, url = endpoints(serving, mirror_log)
                current = status(url)
                assert current["height"] == args.to_height and current["synced"], current
                if expected is None:
                    expected = current
                assert current["hash"] == expected["hash"], current
                peers.extend(("--peer", peer))
            results = []
            for name, binary in (("baseline", args.baseline), ("candidate", args.candidate)):
                receiving = directory / name
                snapshot(mirror, receiving)
                starting_prefix(receiving, args.from_height)
                log = args.output / (name + ".log")
                started = time.monotonic()
                latencies = []
                with launch(binary.resolve(), args.mmx_root.resolve(), receiving, log,
                            "--rollback-to", str(args.from_height), "--sync-once",
                            *peers) as process:
                    _, rpc = endpoints(process, log)
                    while process.poll() is None:
                        before = time.monotonic()
                        try:
                            status(rpc)
                            latencies.append(time.monotonic() - before)
                        except (OSError, urllib.error.URLError):
                            # --sync-once closes its HTTP listener before the
                            # remaining peer/runtime threads finish joining.
                            process.wait(timeout=30)
                        if time.monotonic() - started > 1800:
                            raise TimeoutError("benchmark sync exceeded 30 minutes")
                        time.sleep(0.25)
                    if process.returncode:
                        raise RuntimeError(log.read_text())
                elapsed = time.monotonic() - started
                assert selected_hash(receiving) == expected["hash"]
                requests = re.findall(r"(\d+) body requests, (\d+) retries", log.read_text())
                if requests:
                    # No archive suffix is available to mask a missing network
                    # fetch, and all three mirrors advertise identical content.
                    downloads, retries = map(int, requests[-1])
                    assert downloads == args.to_height - args.from_height + retries, (downloads, retries)
                latencies.sort()
                result = dict(
                    binary=name, seconds=round(elapsed, 3),
                    blocks_per_second=round((args.to_height - args.from_height) / elapsed, 2),
                    rpc_samples=len(latencies),
                    rpc_p95_seconds=round(latencies[int((len(latencies) - 1) * .95)], 3)
                    if latencies else None,
                    hash=expected["hash"],
                    body_requests=int(requests[-1][0]) if requests else None,
                    timeout_retries=int(requests[-1][1]) if requests else None,
                )
                results.append(result)
                print(json.dumps(result), flush=True)
            (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
