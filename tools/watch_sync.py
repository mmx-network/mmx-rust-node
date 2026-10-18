#!/usr/bin/env python3
"""Read-only mainnet sync/RPC monitor, including sustained live advancement."""
import argparse
import json
from pathlib import Path
import time
import urllib.request


def load(url, body=None):
    request = urllib.request.Request(
        url,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def rpc(url, method, **params):
    response = load(
        url, {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    )
    if "error" in response:
        raise RuntimeError(response["error"])
    return response["result"]


def digest(value):
    return value.upper() if isinstance(value, str) else bytes(value).hex().upper()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rpc", default="http://127.0.0.1:12380/rpc")
    parser.add_argument("--native-wapi", default="http://127.0.0.1:11380/wapi")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--interval", type=float, default=45)
    parser.add_argument("--stall-seconds", type=float, default=300)
    parser.add_argument("--live-seconds", type=float, default=300)
    parser.add_argument("--live-blocks", type=int, default=3)
    parser.add_argument("--samples", type=int, default=0, help="0 runs until live checks pass")
    args = parser.parse_args()
    if min(args.interval, args.stall_seconds, args.live_seconds, args.live_blocks) <= 0 or args.samples < 0:
        parser.error("durations/block count must be positive; samples must be nonnegative")
    args.native_wapi = args.native_wapi.rstrip("/")
    args.output.parent.mkdir(parents=True, exist_ok=True)

    def emit(**record):
        record["unix"] = time.time()
        line = json.dumps(record)
        with args.output.open("a") as output:
            output.write(line + "\n")
        print(line, flush=True)

    changed = time.monotonic()
    previous = None
    first = None
    live = None
    genesis = None
    failures = 0
    sample = 0
    milestones = set()
    emit(phase="start", rpc=args.rpc, native_wapi=args.native_wapi)
    while True:
        began = time.monotonic()
        try:
            status = rpc(args.rpc, "get_status")
            latency = time.monotonic() - began
            native = load(args.native_wapi + "/node/info")
            height = status["height"]
            if genesis is None:
                genesis = digest(rpc(args.rpc, "get_genesis_hash"))
                if genesis != digest(native["genesis_hash"]):
                    raise RuntimeError("native genesis differs")
            header = load(args.native_wapi + f"/header?height={height}")
            # Re-read the same height if a branch switch raced the status call.
            selected = rpc(args.rpc, "get_block_hash", height=height)
            if digest(selected) != digest(header["hash"]):
                raise RuntimeError(f"native selected hash differs at {height}")
            peers = status["sync_peers"]
            if peers > 8 or (status["synced"] and peers < 3):
                raise RuntimeError(f"invalid sync peer count: {peers}")
            now = began + latency
            if first is None:
                first = (now, height)
            if previous is None or previous["height"] != height:
                changed = now
            if not status["synced"] and native["height"] > height and now - changed > args.stall_seconds:
                raise RuntimeError(f"historical sync has not advanced for {now - changed:.1f} seconds")
            for fork in [1_000_000, 5_050_000]:
                if height >= fork + 2 and fork not in milestones:
                    for point in [fork - 1, fork, fork + 1, fork + 2]:
                        actual = digest(rpc(args.rpc, "get_block_hash", height=point))
                        expected = digest(load(args.native_wapi + f"/header?height={point}")["hash"])
                        if actual != expected:
                            raise RuntimeError(f"native hard-fork hash differs at {point}")
                    milestones.add(fork)
                    emit(phase="hardfork_verified", height=fork)
            current = (now, height)
            if status["synced"] and native["is_synced"] and abs(native["height"] - height) <= 2:
                if live is None or (previous and height < previous["height"]):
                    live = current
            else:
                live = None
            emit(
                phase="sample", status=status, rpc_seconds=latency,
                native_height=native["height"], checked_hash=digest(selected), native_matches=True,
                blocks_per_second=(height - first[1]) / max(now - first[0], 1e-9),
                live_seconds=0 if live is None else now - live[0],
                live_blocks=0 if live is None else height - live[1],
            )
            previous = status
            failures = 0
            sample += 1
            if live and now - live[0] >= args.live_seconds and height - live[1] >= args.live_blocks:
                emit(phase="live_verified", height=height, seconds=now - live[0], blocks=height - live[1])
                return
            if args.samples and sample >= args.samples:
                emit(phase="samples_complete", samples=sample, live_verified=False)
                return
        except Exception as error:
            # Temporary endpoint failures are observations, never authorization
            # to restart a node or declare its process terminal.
            live = None
            failures += 1
            emit(phase="error", error=str(error), consecutive_failures=failures)
            if failures >= 3:
                raise
        time.sleep(max(0, args.interval - (time.monotonic() - began)))


if __name__ == "__main__":
    main()
