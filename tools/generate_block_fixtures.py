#!/usr/bin/env python3
"""Run original native block validation with deterministic serial task scheduling."""
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
native = (args.source / 'src/Node_validate.cpp').read_text()
node = (args.source / 'src/Node.cpp').read_text()


def section(text, start, end):
    return text[text.index(start):text.index(end, text.index(start))]


tx_methods = section(native, 'void Node::execute(\tstd::shared_ptr<const Transaction>', '\nvoid Node::validate_diff_adjust')
foundation = (root / 'tools/reference_transactions.cpp').read_text().split('std::vector<Spec> cases()')[0]
foundation = foundation.replace('// MMX_NATIVE_VALIDATION', tx_methods.replace('Node::', 'ReferenceNode::'))
needle = 'std::unordered_map<addr_t,std::vector<hash_t>> mutate_map;'
foundation = foundation.replace(needle, needle + '''
        std::map<addr_t,std::set<addr_t>> wait_map;
        std::map<addr_t,std::shared_ptr<int>> signal_map;
        void setup_wait(const hash_t&,const addr_t&);
        void wait(const hash_t&)const{} void signal(const hash_t&)const{}
''')
methods = section(native, 'std::shared_ptr<Node::execution_context_t> Node::new_exec_context', '\nexec_result_t Node::validate(std::shared_ptr<const Transaction> tx) const')
methods += section(node, 'std::vector<addr_t> Node::get_all_depends', '\nvnx::optional<Node::proof_data_t> Node::find_best_proof')
methods += section(node, 'uint64_t Node::calc_block_reward', '\nvnx::optional<addr_t> Node::get_vdf_reward_winner')
setup = section(native, 'void Node::execution_context_t::setup_wait', '\nstd::shared_ptr<Node::execution_context_t> Node::new_exec_context')
source = '#include <mmx/Block.hxx>\n#include <mmx/ProofOfSpaceOG.hxx>\n#include <mmx/ProofOfSpaceNFT.hxx>\n' + foundation
source += '\nnamespace mmx {\n' + setup.replace('Node::', 'ReferenceNode::') + '\n}\n'
vdf_method = section(node, 'vnx::optional<addr_t> Node::get_vdf_reward_winner', '\nstd::shared_ptr<const BlockHeader> Node::read_block')
source += (root / 'tools/reference_blocks.cpp').read_text().replace('// MMX_BLOCK_METHODS', methods.replace('Node::', 'BlockReference::')).replace('// MMX_VDF_METHOD', vdf_method.replace('Node::', 'VdfReference::'))
with tempfile.TemporaryDirectory(prefix='mmx-blocks-') as tmp:
    cpp = pathlib.Path(tmp) / 'reference_blocks.cpp'
    cpp.write_text(source)
    binary = pathlib.Path(tmp) / 'reference_blocks'
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
    output = pathlib.Path(tmp) / 'blocks.jsonl'
    economics = pathlib.Path(tmp) / 'economics.tsv'
    vdf = pathlib.Path(tmp) / 'vdf_winners.jsonl'
    subprocess.run([str(binary), str(output), str(economics), str(vdf)], check=True)
    (root / 'crates/mmx-execution/tests/fixtures/blocks.jsonl').write_bytes(output.read_bytes())
    (root / 'crates/mmx-execution/tests/fixtures/economics.tsv').write_bytes(economics.read_bytes())
    (root / 'crates/mmx-execution/tests/fixtures/vdf_winners.jsonl').write_bytes(vdf.read_bytes())
