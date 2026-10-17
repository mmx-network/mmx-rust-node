#!/usr/bin/env python3
"""Compare native BlockHeader methods and contextual scalar checks without node data."""
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
native = (args.source / 'src/BlockHeader.cpp').read_text()
start = native.index('hash_t BlockHeader::calc_hash(')
end = native.index('\nhash_t BlockHeader::calc_content_hash', start)
capture = native[start:end].replace('hash_t BlockHeader::calc_hash(', 'std::vector<uint8_t> HeaderCapture::capture(')
capture = capture.replace('return hash_t(buffer);', 'return buffer;')
node = (args.source / 'src/Node_validate.cpp').read_text()
start = node.index('\tconst auto prev = find_prev(block);')
end = node.index('\tif(block->static_cost > params->max_block_size)', start)
context = 'void ContextReference::validate(std::shared_ptr<const BlockHeader> block) const {\n' + node[start:end] + '}\n'
source = (root / 'tools/reference_headers.cpp').read_text().replace('// MMX_HEADER_BYTES', capture).replace('// MMX_HEADER_CONTEXT', context)
with tempfile.TemporaryDirectory(prefix='mmx-headers-') as tmp:
    cpp = pathlib.Path(tmp) / 'reference_headers.cpp'
    cpp.write_text(source)
    binary = pathlib.Path(tmp) / 'reference_headers'
    command = [link[0], '-std=c++17', '-O0', *flags['CXX_DEFINES'], *flags['CXX_INCLUDES'], str(cpp)]
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
    headers = pathlib.Path(tmp) / 'headers.jsonl'
    proofs = pathlib.Path(tmp) / 'proof_records.jsonl'
    subprocess.run([str(binary), str(headers), str(proofs)], check=True)
    destination = root / 'crates/mmx-consensus/tests/fixtures'
    (destination / 'headers.jsonl').write_bytes(headers.read_bytes())
    (destination / 'proof_records.jsonl').write_bytes(proofs.read_bytes())
