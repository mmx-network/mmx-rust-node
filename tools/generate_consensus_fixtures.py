#!/usr/bin/env python3
"""Generate native consensus fixtures using the existing C++ build, without modifying it."""
import argparse
import pathlib
import shlex
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=pathlib.Path, default=pathlib.Path.home() / 'workspace/mmx-node')
parser.add_argument('--build', type=pathlib.Path)
args = parser.parse_args()
root = pathlib.Path(__file__).resolve().parent.parent
build = (args.build or args.source / 'build').resolve()
target = build / 'test/CMakeFiles/vm_engine_tests.dir'
flags = {}
for line in (target / 'flags.make').read_text().splitlines():
    if ' = ' in line:
        key, value = line.split(' = ', 1)
        flags[key] = shlex.split(value)
link = shlex.split((target / 'link.txt').read_text())
with tempfile.TemporaryDirectory(prefix='mmx-consensus-') as directory:
    binary = pathlib.Path(directory) / 'reference_consensus'
    command = [link[0], '-std=c++17', '-O0', *flags['CXX_DEFINES'], *flags['CXX_INCLUDES'], str(root / 'tools/reference_consensus.cpp')]
    i = 1
    while i < len(link):
        arg = link[i]
        if arg == '-o':
            i += 2
            continue
        if not arg.endswith('engine_tests.cpp.o'):
            command.append(arg)
        i += 1
    command.extend(['-o', str(binary)])
    subprocess.run(command, cwd=build / 'test', check=True)
    subprocess.run([str(binary), str(root / 'crates/mmx-consensus/tests/fixtures/native.tsv')], check=True)
