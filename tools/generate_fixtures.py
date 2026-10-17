#!/usr/bin/env python3
"""Generate MMX-specific fixtures using an already built C++ reference node.

Uses CMake's actual include and link configuration; never changes the reference
source tree or its database. Ordinary cargo tests use the checked-in fixtures.
"""
import argparse
import pathlib
import shlex
import subprocess
import tempfile
from composite_cases import cases, gas_boundaries
from feature_cases import cases as feature_cases

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source", type=pathlib.Path, default=pathlib.Path.home() / "workspace/mmx-node")
parser.add_argument("--build", type=pathlib.Path)
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parent.parent
build = (args.build or args.source / "build").resolve()
target = build / "test/CMakeFiles/vm_engine_tests.dir"
flags = {}
for line in (target / "flags.make").read_text().splitlines():
    if " = " in line:
        key, value = line.split(" = ", 1)
        flags[key] = shlex.split(value)
link = shlex.split((target / "link.txt").read_text())
with tempfile.TemporaryDirectory(prefix="mmx-reference-") as directory:
    def compile_oracle(source):
        binary = pathlib.Path(directory) / source.removesuffix(".cpp")
        command = [link[0], "-std=c++17", "-O0", *flags["CXX_DEFINES"], *flags["CXX_INCLUDES"]]
        command.append(str(root / "tools" / source))
        i = 1
        while i < len(link):
            arg = link[i]
            if arg == "-o":
                i += 2
                continue
            if not arg.endswith("engine_tests.cpp.o"):
                command.append(arg)
            i += 1
        command.extend(["-o", str(binary)])
        subprocess.run(command, cwd=build / "test", check=True)
        return binary

    output = root / "crates/mmx-vm/tests/fixtures"
    output.mkdir(parents=True, exist_ok=True)
    binary = compile_oracle("reference_fixtures.cpp")
    subprocess.run([str(binary), str(output)], check=True)
    binary = compile_oracle("reference_composites.cpp")
    script = pathlib.Path(directory) / "cases.txt"

    def run_cases(scenarios, destination, database_dir):
        script.write_text("\n".join(f"[{name}]\n" + "\n".join(commands) for name, commands in scenarios) + "\n")
        subprocess.run([str(binary), str(script), str(destination), str(database_dir)], check=True)

    fixture = output / "composites.tsv"
    run_cases(cases(), fixture, pathlib.Path(directory) / "baseline-db")
    baseline = fixture.read_text()
    boundary_fixture = pathlib.Path(directory) / "boundaries.tsv"
    run_cases(gas_boundaries([line.split("\t") for line in baseline.splitlines()]), boundary_fixture, pathlib.Path(directory) / "boundary-db")
    fixture.write_text(baseline + boundary_fixture.read_text())

    fixture = output / "features.tsv"
    run_cases(feature_cases(), fixture, pathlib.Path(directory) / "features-db")
    baseline = fixture.read_text()
    # The format/type matrix has its own exhaustive cases. Sweep gas for the
    # focused execution traces, avoiding millions of redundant large-int runs.
    rows = [line.split("\t") for line in baseline.splitlines() if not line.startswith("matrix_")]
    boundaries = []
    for name, commands in gas_boundaries(rows):
        # Large conversions cost 250k: include their end boundary and the
        # significant internal checkpoints rather than every 257-unit point.
        budget = int(name.rsplit("_gas", 1)[1])
        if budget > 3000 and budget % 257 == 0:
            continue
        boundaries.append((name, commands))
    run_cases(boundaries, boundary_fixture, pathlib.Path(directory) / "features-boundary-db")
    fixture.write_text(baseline + boundary_fixture.read_text())
    collision = subprocess.check_output([str(binary), '--collision'], text=True).splitlines()
    commands = ['new 0', *[f'write {i+1} {v}' for i, v in enumerate(collision)], 'init',
                'run VERIFY,0,S,1,2,3;RET,0', 'read S', 'new 0',
                'write 1 b:' + '00'*32, 'write 2 b:' + '00'*33, 'write 3 b:' + '00'*64,
                'init', 'run VERIFY,0,S,1,2,3;RET,0']
    run_cases([('signature_cache_zero_entry_evicted', commands)], boundary_fixture, pathlib.Path(directory) / 'collision-db')
    with fixture.open('a') as out:
        out.write(boundary_fixture.read_text())
