#!/usr/bin/env python3
"""Use existing C++ build libraries; never rebuild or open a node database."""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import tempfile

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source',type=Path,default=Path.home()/'workspace/mmx-node')
args=parser.parse_args()
root=Path(__file__).resolve().parent.parent
build=(args.source/'build').resolve()
target=build/'test/CMakeFiles/test_pos_compute.dir'
flags={k:shlex.split(v) for line in (target/'flags.make').read_text().splitlines() if ' = ' in line for k,v in [line.split(' = ',1)]}
link=shlex.split((target/'link.txt').read_text())
output=root/'crates/mmx-farming/tests/native'
output.mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='mmx-farming-reference-') as tmp:
    tmp=Path(tmp)
    proof=json.loads((root/'crates/mmx-farming/tests/fixtures.json').read_text())['proof']['wire']
    (tmp/'proof.vnx').write_bytes(bytes.fromhex(proof))
    command=[link[0],'-std=c++17','-O0',*flags['CXX_DEFINES'],*flags['CXX_INCLUDES'],str(root/'tools/reference_farming.cpp')]
    i=1
    while i<len(link):
        arg=link[i]
        if arg=='-o':i+=2;continue
        if not arg.endswith('test_pos_compute.cpp.o'):command.append(arg)
        i+=1
    command.extend(['-o',str(tmp/'reference')])
    subprocess.run(command,cwd=build/'test',check=True)
    subprocess.run([str(tmp/'reference'),str(tmp/'proof.vnx'),str(output)],check=True)
