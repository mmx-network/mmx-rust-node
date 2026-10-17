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
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def snapshot(source, destination):
    destination.mkdir(parents=True)
    shutil.copy2(source / "identity.key", destination / "identity.key")
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
        mirror = directory / "mirror"
        snapshot(args.source_dir.resolve(), mirror)
        mirror_log = args.output / "mirror.log"
        with launch(args.candidate.resolve(), args.mmx_root.resolve(), mirror,
                    mirror_log, "--offline", "--rollback-to", str(args.to_height)) as serving:
            peer, url = endpoints(serving, mirror_log)
            expected = status(url)
            assert expected["height"] == args.to_height, expected
            results = []
            for name, binary in (("baseline", args.baseline), ("candidate", args.candidate)):
                receiving = directory / name
                snapshot(mirror, receiving)
                log = args.output / (name + ".log")
                started = time.monotonic()
                latencies = []
                with launch(binary.resolve(), args.mmx_root.resolve(), receiving, log,
                            "--rollback-to", str(args.from_height), "--sync-once",
                            "--peer", peer) as process:
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
                latencies.sort()
                result = dict(
                    binary=name, seconds=round(elapsed, 3),
                    blocks_per_second=round((args.to_height - args.from_height) / elapsed, 2),
                    rpc_samples=len(latencies),
                    rpc_p95_seconds=round(latencies[int((len(latencies) - 1) * .95)], 3)
                    if latencies else None,
                    hash=expected["hash"],
                )
                results.append(result)
                print(json.dumps(result), flush=True)
            (args.output / "results.json").write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
