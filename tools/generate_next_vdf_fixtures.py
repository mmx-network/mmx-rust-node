#!/usr/bin/env python3
"""Extract native next-VDF selection over verified work branches."""
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
native=(a.source/'src/Node.cpp').read_text()
start=native.index('std::vector<std::shared_ptr<const VDF_Point>> Node::find_next_vdf_points')
end=native.index('\nstd::set<pubkey_t> Node::get_validators',start)
source=(root/'tools/reference_next_vdf.cpp').read_text().replace('// MMX_NEXT_VDF',native[start:end].replace('Node::','NextReference::'))
rows=[]
rng=random.Random(510204)
for n in range(240):
    count=rng.randrange(1,50);num=rng.choice([1,2,8]);iters=rng.choice([0,100,2**64-10]);maximum=rng.randrange(1,9)
    points=[]
    for i in range(count):
        end=(iters+num*rng.randrange(1,maximum+2))%2**64
        points.append(dict(input=rng.randrange(i+1),output=i+1,start=str((end-num)%2**64),num=num if rng.randrange(5) else num+1,
            prev=41 if rng.randrange(5) else 42,recv_time=i))
    rows.append(dict(name=f'next{n}',iters=str(iters),num=num,maximum=maximum,points=points))
with tempfile.TemporaryDirectory(prefix='mmx-next-vdf-') as tmp:
    tmp=Path(tmp);cpp=tmp/'reference_next_vdf.cpp';cpp.write_text(source);binary=tmp/'reference_next_vdf'
    command=[link[0],'-std=c++17','-O0',*flags['CXX_DEFINES'],*flags['CXX_INCLUDES'],str(cpp)]
    i=1
    while i<len(link):
        arg=link[i]
        if arg=='-o':i+=2;continue
        if not arg.endswith('test_pos_compute.cpp.o'):command.append(arg)
        i+=1
    command.extend(['-o',str(binary)]);subprocess.run(command,cwd=build/'test',check=True)
    inputs=tmp/'cases.jsonl';inputs.write_text(''.join(json.dumps(r)+'\n' for r in rows))
    subprocess.run([str(binary),str(inputs),str(root/'crates/mmx-consensus/tests/fixtures/next_vdf.jsonl')],check=True)
    print(f'{len(rows)} native next-VDF cases')
