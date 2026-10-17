#!/usr/bin/env python3
"""Native space-work, header transition and ancestor lookup comparisons."""
import argparse
import pathlib
import shlex
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=pathlib.Path, default=pathlib.Path.home() / 'workspace/mmx-node')
p.add_argument('--build', type=pathlib.Path)
a = p.parse_args()
root = pathlib.Path(__file__).resolve().parent.parent
build = (a.build or a.source / 'build').resolve()
target = build / 'test/CMakeFiles/test_pos_compute.dir'
flags = {}
for line in (target / 'flags.make').read_text().splitlines():
    if ' = ' in line:
        key, value = line.split(' = ', 1)
        flags[key] = shlex.split(value)
link = shlex.split((target / 'link.txt').read_text())


def section(text, start, end):
    i = text.index(start)
    return text[i:text.index(end, i)]


verify = (a.source / 'src/Node_verify.cpp').read_text()
node = (a.source / 'src/Node.cpp').read_text()
validate = (a.source / 'src/Node_validate.cpp').read_text()
methods = section(verify, 'void Node::verify_proof(std::shared_ptr<const BlockHeader>', '\nvoid Node::verify_vdf(')
methods += section(validate, 'void Node::validate_diff_adjust', '\n\n} // mmx')
history = section(node, 'std::shared_ptr<const BlockHeader> Node::find_prev(\tstd::shared_ptr<const BlockHeader>', '\nbool Node::find_challenge(const uint32_t vdf_height')
history += section(node, 'hash_t Node::get_challenge(std::shared_ptr<const BlockHeader>', '\nstd::pair<uint32_t, hash_t> Node::get_vdf_peak_ex')
candidate = (a.source / 'src/pos/verify_full.cpp').read_text()
candidate = section(candidate, 'std::vector<std::pair<uint32_t, bytes_t<META_BYTES_OUT>>>', '\n\n\n\n} // pos').replace('compute_full(', 'candidate_full(')
# This bounded copy is used only to find inexpensive small-ksize witnesses.
# Expected outputs/acceptance always come from the unmodified linked verifier.
candidate = candidate.replace('matches.emplace_back(Y_i, M_next.size());', 'if(matches.size() >= 20000) throw std::logic_error("candidate size limit");\n matches.emplace_back(Y_i, M_next.size());')
source = (root / 'tools/reference_work.cpp').read_text().replace('// MMX_WORK_METHODS', methods.replace('Node::', 'WorkReference::')).replace('// MMX_HISTORY_METHODS', history.replace('Node::', 'HistoryReference::')).replace('// MMX_CANDIDATE', candidate)
with tempfile.TemporaryDirectory(prefix='mmx-work-') as tmp:
    cpp = pathlib.Path(tmp) / 'reference_work.cpp'
    cpp.write_text(source)
    binary = pathlib.Path(tmp) / 'reference_work'
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
    output = pathlib.Path(tmp) / 'work.jsonl'
    subprocess.run([str(binary), str(output)], check=True)
    (root / 'crates/mmx-consensus/tests/fixtures/work.jsonl').write_bytes(output.read_bytes())
