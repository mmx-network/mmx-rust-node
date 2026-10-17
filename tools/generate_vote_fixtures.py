#!/usr/bin/env python3
"""Native validator vote signatures, membership and queued-count transitions."""
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
node = (a.source / 'src/Node.cpp').read_text()
update = (a.source / 'src/Node_update.cpp').read_text()
membership = node[node.index('std::set<pubkey_t> Node::get_validators'):node.index('\nstd::vector<addr_t> Node::get_all_depends')]
queue = update[update.index('void Node::verify_votes()'):update.index('\nvoid Node::verify_proofs()')]
source = (root / 'tools/reference_votes.cpp').read_text().replace('// MMX_VOTE_METHODS', (membership + queue).replace('Node::', 'VoteReference::'))
rows = []
for delay in (0, 1, 2, 5, 20):
    for limit in (0, 1, 2, 5, 1000):
        for maximum in (0, 1, 3, 11):
            headers = [dict(id=n+1, prev=n if n else 0, height=n, keys=([1+n%3, 9] if n%4 else [])) for n in range(10)]
            rows.append(dict(kind='history', name=f'd{delay}_h{limit}_v{maximum}', delay=delay, max_history=limit, max_validators=maximum, headers=headers, tip=10))
rows.append(dict(kind='history', name='missing_ancestor', delay=1, max_history=1000, max_validators=11, headers=[dict(id=10, prev=9, height=9, keys=[1]),dict(id=9,prev=8,height=8,keys=[2])], tip=10))
def register(block=1, proof=50, verified=True, validators=(1,2), challenge=1):
    return dict(type='register', block=block, proof=proof, challenge=challenge, verified=verified, validators=list(validators))
def vote(block=1, key=1, at=0):
    return dict(type='vote', block=block, key=key, at=at)
def advance(at):
    return dict(type='advance', at=at)
cases = [
    [register(),vote(),vote(),vote(key=2),vote(key=3),advance(100)],
    [vote(),advance(30),register(),vote(key=2),advance(31)],
    [vote(),advance(31),register()],
    [register(verified=False),vote(),advance(30),dict(type='verify',block=1)],
    [register(),vote(),dict(type='proof',challenge=1,proof=1),vote(key=2),vote(),advance(100)],
    [dict(type='proof',challenge=1,proof=1),register(),vote(),vote(key=3)],
    [register(),vote(),register(validators=(3,)),vote(key=3),vote(key=2)],
    [register(block=1),register(block=2),vote(block=1),vote(block=2),vote(block=1)],
    [vote(at=100),advance(50),register(),advance(131)],
]
rng = random.Random(224)
for run in range(60):
    events=[]
    for block in range(1,5):
        events.append(register(block=block, proof=rng.choice([1,50,256]), verified=rng.choice([True,False]), validators=rng.sample([1,2,3],rng.randrange(4))))
    now=0
    for _ in range(24):
        now += rng.randrange(8)
        kind=rng.randrange(6)
        if kind<4: events.append(vote(block=rng.randrange(1,7),key=rng.randrange(1,5),at=now))
        elif kind==4: events.append(dict(type='verify',block=rng.randrange(1,5)))
        else: events.append(advance(now))
    cases.append(events)
for n, events in enumerate(cases):
    rows.append(dict(kind='queue', name=f'queue{n}', interval=10, events=events))
with tempfile.TemporaryDirectory(prefix='mmx-votes-') as tmp:
    tmp = Path(tmp)
    cpp = tmp / 'reference_votes.cpp'
    cpp.write_text(source)
    binary = tmp / 'reference_votes'
    command = [link[0], '-std=c++17', '-O0', *flags['CXX_DEFINES'], *flags['CXX_INCLUDES'], str(cpp)]
    i=1
    while i<len(link):
        arg=link[i]
        if arg=='-o': i+=2;continue
        if not arg.endswith('test_pos_compute.cpp.o'): command.append(arg)
        i+=1
    command.extend(['-o',str(binary)])
    subprocess.run(command,cwd=build/'test',check=True)
    inputs=tmp/'cases.jsonl'
    inputs.write_text(''.join(json.dumps(row)+'\n' for row in rows))
    output=root/'crates/mmx-execution/tests/fixtures/votes.jsonl'
    subprocess.run([str(binary),str(inputs),str(output),str(root/'crates/mmx-execution/tests/fixtures/chain.jsonl')],check=True)
    # Deduplicate identical native wire blobs; expected outcomes remain intact.
    native=[json.loads(line) for line in output.read_text().splitlines()]
    histories={}
    vote_ids={}
    vote_wires=[]
    for row in native:
        if row['kind']=='history':
            snapshot=tuple(row['wires'])
            if snapshot in histories:
                row['seed']=histories[snapshot]
                del row['wires']
            else:
                histories[snapshot]=row['name']
        if row['kind']=='queue':
            for event in row['events']:
                if 'wire' in event:
                    wire=event.pop('wire')
                    if wire not in vote_ids:
                        vote_ids[wire]=len(vote_wires)
                        vote_wires.append(wire)
                    event['vote']=vote_ids[wire]
    native.append(dict(kind='queue_wires', records=vote_wires))
    output.write_text(''.join(json.dumps(row,separators=(',',':'))+'\n' for row in native))
    print(f'{len(native)-1} native vote cases')
