#!/usr/bin/env python3
"""Native ProofOfTime admission, SHA work, point hashes and output linkage."""
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
admission = section(verify, 'void Node::verify_vdf(', '\nvoid Node::verify_vdf_cpu')
# Keep every admission check; remove only asynchronous scheduling/trust bookkeeping.
admission = admission[:admission.index('\tvdf_threads->add_task')] + '}\n'
cpu = section(verify, 'void Node::verify_vdf_cpu', '\nvoid Node::verify_vdf_success')
# Exercise the unmodified hardware-dispatch path and its native fallback. The
# latter changes only CPU feature selection, never hashing or checked outputs.
fallback = cpu.replace('verify_vdf_cpu(', 'verify_vdf_fallback(').replace('sha256_ni_available()', 'false').replace('sha256_arm_available()', 'false')
history = section(node, 'std::shared_ptr<const BlockHeader> Node::find_prev(\tstd::shared_ptr<const BlockHeader>', '\nbool Node::find_challenge(')
history += section(node, 'bool Node::find_challenge(std::shared_ptr<const BlockHeader>', '\nbool Node::find_challenge(const uint32_t vdf_height')
history += section(node, 'hash_t Node::get_challenge(std::shared_ptr<const BlockHeader>', '\nuint64_t Node::get_time_diff(')
history += section(node, 'uint64_t Node::get_time_diff(', '\nstd::pair<uint32_t, hash_t> Node::get_vdf_peak_ex')
finder = section(node, 'std::vector<std::shared_ptr<const VDF_Point>>\nNode::find_vdf_points(', '\nstd::vector<std::shared_ptr<const VDF_Point>> Node::find_next_vdf_points(')
space = section(verify, 'void Node::verify_proof(std::shared_ptr<const BlockHeader>', '\nvoid Node::verify_vdf(')
validate = (a.source / 'src/Node_validate.cpp').read_text()
space += section(validate, 'void Node::validate_diff_adjust', '\n\n} // mmx')
methods = (admission + cpu + fallback + history + finder + space).replace('Node::', 'VdfReference::')
capture = ''
for name in ['ProofOfTime', 'VDF_Point']:
    native = (a.source / f'src/{name}.cpp').read_text()
    body = section(native, f'hash_t {name}::calc_hash()', '\nhash_t ' if name == 'ProofOfTime' else '\n\n\n} // mmx')
    capture += body.replace(f'hash_t {name}::calc_hash()', f'std::vector<uint8_t> {name}Capture::capture()').replace('return hash_t(buffer);', 'return buffer;')
source = (root / 'tools/reference_vdf.cpp').read_text().replace('// MMX_VDF_METHODS', methods).replace('// MMX_VDF_BYTES', capture)
with tempfile.TemporaryDirectory(prefix='mmx-vdf-') as tmp:
    cpp = pathlib.Path(tmp) / 'reference_vdf.cpp'
    cpp.write_text(source)
    binary = pathlib.Path(tmp) / 'reference_vdf'
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
    output = pathlib.Path(tmp) / 'vdf.jsonl'
    subprocess.run([str(binary), str(output), str(root / 'crates/mmx-consensus/tests/fixtures/work.jsonl')], check=True)
    (root / 'crates/mmx-consensus/tests/fixtures/vdf.jsonl').write_bytes(output.read_bytes())
