#!/usr/bin/env python3
"""Run a packaged CLI without accessing the user's wallets or the network."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

command = sys.argv[1:]
if not command:
    raise SystemExit("usage: check-packaged-cli.py EXECUTABLE [PREFIX_ARGS]")
with tempfile.TemporaryDirectory(prefix="mmx-packaged-cli-") as home:
    directory = Path(home, "wallets")
    env = {**os.environ, "HOME": home, "USERPROFILE": home, "MMX_HOME": str(directory)}
    for args in [["--help"], ["--version"], ["list", "--json"]]:
        result = subprocess.run(command + args, env=env, capture_output=True, text=True, timeout=60, check=True)
        if args[0] == "list":
            data = json.loads(result.stdout)
            assert Path(data["wallet_directory"]) == directory
            assert data["wallets"] == [] and data["unreadable"] == []
        else:
            assert "mmxwallet" in result.stdout
    assert not directory.exists(), "listing must not create a wallet directory"
print("Packaged mmxwallet: help, version, and isolated wallet listing passed")
