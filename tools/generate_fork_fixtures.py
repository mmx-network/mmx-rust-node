#!/usr/bin/env python3
"""Compare native fork choice over competing roots, votes and proof states."""
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
native = (a.source / 'src/Node.cpp').read_text()
start = native.index('std::shared_ptr<Node::fork_t> Node::find_best_fork() const')
method = native[start:native.index('\nstd::vector<std::shared_ptr<Node::fork_t>> Node::get_fork_line', start)]
source = (root / 'tools/reference_forks.cpp').read_text().replace('// MMX_FORK_CHOICE', method.replace('Node::', 'ForkReference::'))

def hash_value(n):
    return n.to_bytes(32, 'little').hex()

def candidate(n, parent=0, height=1, weight=1, votes=0, verified=True, invalid=False, anchor_weight=0):
    return dict(hash=hash_value(n), prev=hash_value(parent), height=height, total_weight=str(weight), votes=votes, proof_verified=verified, invalid=invalid,
                anchor=dict(hash=hash_value(parent), total_weight=str(anchor_weight)) if height == 1 else None)

cases = []
def add(name, candidates, root_height=0, sync=None):
    cases.append(dict(name=name, root=hash_value(0), root_height=root_height, sync_pending=sync, candidates=candidates))

for weight in (0, 1, 2, 2**128-1):
    for votes in (0, 1, 2**32-1):
        for anchor_weight in (0, 1):
            add(f'weight{weight}_votes{votes}_root{anchor_weight}', [candidate(1, weight=2, votes=1), candidate(256, parent=99, weight=weight, votes=votes, anchor_weight=anchor_weight)])
add('wrapping_votes', [candidate(1, votes=2**32-1), candidate(2, 1, 2, votes=1), candidate(3, votes=2)])
add('equal_height_arrival_order', [candidate(256), candidate(1)])
add('invalid_parent', [candidate(1, invalid=True), candidate(2, 1, 2, votes=99)])
add('unverified_parent', [candidate(1, verified=False), candidate(2, 1, 2, votes=99)])
add('missing_parent', [candidate(1, 999, 2, votes=99)])
add('missing_anchor', [dict(candidate(1), anchor=None)])

rng = random.Random(462016)
for run in range(400):
    height = rng.choice([0, 1, 5, 100])
    nodes = []
    weights = {hash_value(0): 0, hash_value(99): 1, hash_value(98): 2**127}
    for n in range(1, rng.randrange(2, 28)):
        parent = rng.choice(nodes) if nodes and rng.randrange(4) else None
        prev = parent['hash'] if parent else rng.choice(list(weights))
        node = candidate(n+1000, height=(parent['height']+1 if parent else height+1), weight=rng.choice([0, 1, 2, 2**127, 2**128-1]),
                         votes=rng.choice([0, 0, 1, 10, 2**32-1]), verified=rng.randrange(5)!=0, invalid=rng.randrange(13)==0)
        node['prev'] = prev
        node['anchor'] = dict(hash=prev, total_weight=str(weights[prev])) if not parent and rng.randrange(10) else None
        nodes.append(node)
    rng.shuffle(nodes)
    add(f'random{run}', nodes, height, rng.choice([None, None, 0, height, height+1]))

# Native header lookup is shared: one sibling can supply an anchor for another.
for row in cases:
    anchors = {node['anchor']['hash']: node['anchor'] for node in row['candidates'] if node['anchor']}
    for node in row['candidates']:
        if node['height'] == row['root_height'] + 1:
            node['anchor'] = anchors.get(node['prev'])

with tempfile.TemporaryDirectory(prefix='mmx-forks-') as tmp:
    tmp = Path(tmp)
    cpp = tmp / 'reference_forks.cpp'
    cpp.write_text(source)
    binary = tmp / 'reference_forks'
    command = [link[0], '-std=c++17', '-O0', *flags['CXX_DEFINES'], *flags['CXX_INCLUDES'], str(cpp)]
    i = 1
    while i < len(link):
        arg = link[i]
        if arg == '-o':
            i += 2
            continue
        if not arg.endswith('test_pos_compute.cpp.o'):
            command.append(arg)
        i += 1
    command.extend(['-o', str(binary)])
    subprocess.run(command, cwd=build / 'test', check=True)
    inputs = tmp / 'cases.jsonl'
    inputs.write_text(''.join(json.dumps(row) + '\n' for row in cases))
    output = root / 'crates/mmx-execution/tests/fixtures/forks.jsonl'
    subprocess.run([str(binary), str(inputs), str(output)], check=True)
    print(f'{len(cases)} native fork-choice cases')
