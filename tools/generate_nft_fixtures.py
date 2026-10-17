#!/usr/bin/env python3
"""Native NFT target calls, address conversion and block reward validation."""
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
target = build / 'test/CMakeFiles/vm_engine_tests.dir'
flags = {}
for line in (target / 'flags.make').read_text().splitlines():
    if ' = ' in line:
        key, value = line.split(' = ', 1)
        flags[key] = shlex.split(value)
link = shlex.split((target / 'link.txt').read_text())


def section(text, start, end):
    i = text.index(start)
    return text[i:text.index(end, i)]


api = (a.source / 'src/Node_api.cpp').read_text()
methods = section(api, 'vnx::Variant Node::call_contract(', '\nuint128 Node::get_total_supply(')
methods += section(api, 'addr_t Node::get_plot_nft_target(', '\noffer_data_t Node::get_offer(')
native = (a.source / 'src/Node_validate.cpp').read_text()
reward = section(native, '\tvnx::optional<addr_t> reward_contract;', '\n\tif(block->height % params->vdf_reward_interval')
methods += '\nvoid Node::validate_reward(std::shared_ptr<const Block> block) const {\n' + reward + '\n}\n'
source = (root / 'tools/reference_nft.cpp').read_text().replace('// MMX_NFT_METHODS', methods.replace('Node::', 'NftReference::'))
with tempfile.TemporaryDirectory(prefix='mmx-nft-') as tmp:
    cpp = pathlib.Path(tmp) / 'reference_nft.cpp'
    cpp.write_text(source)
    binary = pathlib.Path(tmp) / 'reference_nft'
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
    output = pathlib.Path(tmp) / 'nft.jsonl'
    subprocess.run([str(binary), str(output), str(a.source.resolve()), str(root / 'crates/mmx-consensus/tests/fixtures/work.jsonl')], check=True)
    (root / 'crates/mmx-execution/tests/fixtures/nft.jsonl').write_bytes(output.read_bytes())
