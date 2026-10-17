#!/usr/bin/env python3
"""Snapshot protocol declarations and native type identities from mmx-node.

Normal builds use this snapshot and the vnx-rust submodule; C++ is only a
reference when deliberately refreshing the schema and identity fixtures.
"""
import argparse
import json
import pathlib
import re
import shutil
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=pathlib.Path, default=pathlib.Path.home() / "workspace/mmx-node")
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parent.parent
source = args.source.resolve()
inputs = [
    ("vnx-base/interface", "vnx-base/interface"),
    ("vnx-base/modules", "vnx-base/modules"),
    ("vnx-addons/interface", "vnx-addons/interface"),
    ("vnx-addons/modules", "vnx-addons/modules"),
    ("interface", "mmx/interface"),
    ("modules", "mmx/modules"),
]
count = 0
for origin, destination in inputs:
    files = sorted((source / origin).rglob("*.vni"))
    if not files:
        raise SystemExit(f"No VNI declarations found in {source / origin}")
    target = root / "schema" / destination
    expected = set()
    for path in files:
        relative = path.relative_to(source / origin)
        expected.add(relative)
        out = target / relative
        out.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, out)
        count += 1
    for path in target.rglob("*.vni"):
        if path.relative_to(target) not in expected:
            path.unlink()

def revision(directory):
    return subprocess.check_output(["git", "-C", str(directory), "rev-parse", "HEAD"], text=True).strip()

provenance = {
    "mmx-node": revision(source),
    "vnx-base": revision(source / "vnx-base"),
    "vnx-addons": revision(source / "vnx-addons"),
}
(root / "schema/origin.json").write_text(json.dumps(provenance, indent=2) + "\n")
hashes = {}
for directory in ["generated/src", "generated/contract/src", "generated/operation/src", "generated/solution/src", "generated/vm/src", "vnx-base/generated/src", "vnx-addons/generated/src"]:
    for path in sorted((source / directory).glob("*.cpp")):
        text = path.read_text()
        for name, type_hash, code_hash in re.findall(r'type_code->name = "([^"]+)";\s*type_code->type_hash = vnx::Hash64\(0x([0-9a-fA-F]+)ull\);\s*type_code->code_hash = vnx::Hash64\(0x([0-9a-fA-F]+)ull\);', text):
            pair = (int(type_hash, 16), int(code_hash, 16))
            if name in hashes and hashes[name] != pair:
                raise SystemExit(f"Conflicting native type identities for {name}")
            hashes[name] = pair
if not hashes:
    raise SystemExit("No generated C++ type identities found")
fixture = root / "crates/mmx-types/tests/fixtures/native_hashes.tsv"
fixture.parent.mkdir(parents=True, exist_ok=True)
fixture.write_text("".join(f"{name}\t{t:016x}\t{c:016x}\n" for name, (t, c) in sorted(hashes.items())))
print(f"Copied {count} VNI files and {len(hashes)} native type/code hash pairs")
