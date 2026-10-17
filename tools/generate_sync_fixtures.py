#!/usr/bin/env python3
"""Extract native root commit policy and compare proof-density/sync boundaries."""
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
native = (a.source / 'src/Node_update.cpp').read_text()
start = native.index('\t\tuint64_t total_proofs = 0;')
end = native.index('\n\t}\n\tconst auto root = get_root();', start)
method = native[start:end]
source = (root / 'tools/reference_sync.cpp').read_text().replace('// MMX_ROOT_POLICY', method)
cases=[]
rng=random.Random(671403)
for n in range(300):
    height=rng.choice([0,1,1000,2**32-500])
    count=rng.randrange(1,80)
    vdf=rng.choice([0,1000,2**32-500])
    line=[]
    for i in range(count):
        vdf=(vdf+rng.randrange(1,5)) % 2**32
        line.append(dict(height=height+i+1,vdf_height=vdf,proofs=rng.randrange(0,8)))
    cases.append(dict(name=f'policy{n}',root=dict(height=height,vdf_height=(line[0]['vdf_height']-1)%2**32,proofs=0),line=line,
        threshold=rng.choice([0,1,80,100,101,2**32-1]),delay=rng.choice([0,1,24,100,2**32-1]),future=rng.choice([0,1,10,100,2**32-1]),
        average=rng.choice([1,3,2**31,2**32-1]),synced=bool(rng.randrange(2)),sync_pos=height+rng.randrange(count+2),pending=rng.sample([b['height'] for b in line],rng.randrange(min(count,4)))))
# Avoid native division by zero (undefined behavior); Rust rejects it explicitly.
cases=[c for c in cases if ((c['line'][-1]['vdf_height']-c['root']['vdf_height'])*c['average'])%2**32]
with tempfile.TemporaryDirectory(prefix='mmx-sync-') as tmp:
    tmp=Path(tmp)
    cpp=tmp/'reference_sync.cpp'; cpp.write_text(source)
    binary=tmp/'reference_sync'
    command=[link[0],'-std=c++17','-O0',*flags['CXX_DEFINES'],*flags['CXX_INCLUDES'],str(cpp)]
    i=1
    while i<len(link):
        arg=link[i]
        if arg=='-o': i+=2; continue
        if not arg.endswith('test_pos_compute.cpp.o'): command.append(arg)
        i+=1
    command.extend(['-o',str(binary)])
    subprocess.run(command,cwd=build/'test',check=True)
    inputs=tmp/'cases.jsonl'; inputs.write_text(''.join(json.dumps(r)+'\n' for r in cases))
    output=root/'crates/mmx-execution/tests/fixtures/sync.jsonl'
    subprocess.run([str(binary),str(inputs),str(output)],check=True)
    print(f'{len(cases)} native root advancement cases')
