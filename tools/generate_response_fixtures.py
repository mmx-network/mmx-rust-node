#!/usr/bin/env python3
"""Native farmer proof response hashes, authentication and mutation cases."""
import argparse
import json
from pathlib import Path
import random
import shlex
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, default=Path.home() / 'workspace/mmx-node')
p.add_argument('--build', type=Path)
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
build = (a.build or a.source / 'build').resolve()
target = build / 'test/CMakeFiles/test_pos_compute.dir'
flags = {}
for line in (target / 'flags.make').read_text().splitlines():
    if ' = ' in line:
        key, value = line.split(' = ', 1)
        flags[key] = shlex.split(value)
link = shlex.split((target / 'link.txt').read_text())
source=(root/'tools/reference_responses.cpp').read_text()
with tempfile.TemporaryDirectory(prefix='mmx-responses-') as tmp:
    tmp=Path(tmp); cpp=tmp/'reference_responses.cpp';cpp.write_text(source);binary=tmp/'reference_responses'
    command=[link[0],'-std=c++17','-O0',*flags['CXX_DEFINES'],*flags['CXX_INCLUDES'],str(cpp)]
    i=1
    while i<len(link):
        arg=link[i]
        if arg=='-o': i+=2;continue
        if not arg.endswith('test_pos_compute.cpp.o'):command.append(arg)
        i+=1
    command.extend(['-o',str(binary)])
    subprocess.run(command,cwd=build/'test',check=True)
    subprocess.run([str(binary),str(root/'crates/mmx-execution/tests/fixtures/chain.jsonl'),str(root/'crates/mmx-consensus/tests/fixtures/responses.jsonl')],check=True)
