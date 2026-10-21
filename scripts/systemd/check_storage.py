#!/usr/bin/env python3
"""Check free space before node startup without reading ledger contents."""
import os
from pathlib import Path
import sys


def main():
    if len(sys.argv) != 3 or not Path(sys.argv[1]).is_absolute():
        raise ValueError("usage: check_storage.py /absolute/path minimum_bytes")
    minimum = int(sys.argv[2])
    if minimum < 0:
        raise ValueError("minimum free bytes must not be negative")
    usage = os.statvfs(sys.argv[1])
    available = usage.f_bavail * usage.f_frsize
    if available < minimum:
        raise ValueError(f"only {available} bytes free; node startup requires {minimum}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as error:
        sys.exit(str(error))
