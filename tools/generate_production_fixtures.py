#!/usr/bin/env python3
"""Find a stronger native proof using existing C++ libraries, without node data."""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, default=Path.home() / 'workspace/mmx-node')
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
build = (a.source / 'build').resolve()
target = build / 'test/CMakeFiles/test_pos_compute.dir'
flags = {k: shlex.split(v) for line in (target / 'flags.make').read_text().splitlines()
         if ' = ' in line for k, v in [line.split(' = ', 1)]}
link = shlex.split((target / 'link.txt').read_text())
verify = (a.source / 'src/Node_verify.cpp').read_text()
methods = verify[verify.index('template<typename T>\nvoid Node::verify_proof_impl('):verify.index('\nvoid Node::verify_vdf(')]
candidate = (a.source / 'src/pos/verify_full.cpp').read_text()
candidate = candidate[candidate.index('std::vector<std::pair<uint32_t, bytes_t<META_BYTES_OUT>>>'):candidate.index('\n\n\n\n} // pos')]
candidate = candidate.replace('compute_full(', 'candidate_full(').replace(
    'matches.emplace_back(Y_i, M_next.size());',
    'if(matches.size() >= 20000) throw std::logic_error("candidate size limit");\n matches.emplace_back(Y_i, M_next.size());')
source = (root / 'tools/reference_production.cpp').read_text().replace(
    '// MMX_PROOF_METHODS', methods.replace('Node::', 'ProductionReference::')).replace('// MMX_CANDIDATE', candidate)
with tempfile.TemporaryDirectory(prefix='mmx-production-') as directory:
    directory = Path(directory)
    cpp = directory / 'reference.cpp'
    cpp.write_text(source)
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
    binary = directory / 'reference'
    command.extend(['-o', str(binary)])
    subprocess.run(command, cwd=build / 'test', check=True)
    output = directory / 'production.json'
    subprocess.run([str(binary), str(root / 'crates/mmx-execution/tests/fixtures/chain.jsonl'), str(output)], check=True)
    row = json.loads(output.read_text())
    row['reference_commit'] = subprocess.check_output(['git', '-C', str(a.source), 'rev-parse', 'HEAD'], text=True).strip()
    destination = root / 'crates/mmx-node/tests/fixtures/production.json'
    destination.parent.mkdir(exist_ok=True)
    destination.write_text(json.dumps(row, indent=2) + '\n')
