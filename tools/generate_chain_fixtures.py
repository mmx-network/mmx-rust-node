#!/usr/bin/env python3
"""Native network genesis and proof-verified ledger extensions for chain tests."""
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


node = (a.source / 'src/Node.cpp').read_text()
validate = (a.source / 'src/Node_validate.cpp').read_text()
verify = (a.source / 'src/Node_verify.cpp').read_text()
tx_methods = section(validate, 'void Node::execute(\tstd::shared_ptr<const Transaction>', '\nvoid Node::validate_diff_adjust')
foundation = (root / 'tools/reference_transactions.cpp').read_text().split('std::vector<Spec> cases()')[0]
foundation = foundation.replace('// MMX_NATIVE_VALIDATION', tx_methods.replace('Node::', 'ReferenceNode::'))
foundation = foundation.replace('std::unordered_map<addr_t,std::vector<hash_t>> mutate_map;', '''std::unordered_map<addr_t,std::vector<hash_t>> mutate_map;
    std::map<addr_t,std::set<addr_t>> wait_map; std::map<addr_t,std::shared_ptr<int>> signal_map;
    void setup_wait(const hash_t&,const addr_t&); void wait(const hash_t&)const{} void signal(const hash_t&)const{}
''')
methods = section(validate, 'std::shared_ptr<Node::execution_context_t> Node::new_exec_context', '\nexec_result_t Node::validate(std::shared_ptr<const Transaction> tx) const')
methods += section(node, 'std::vector<addr_t> Node::get_all_depends', '\nvnx::optional<Node::proof_data_t> Node::find_best_proof')
methods += section(node, 'uint64_t Node::calc_block_reward', '\nvnx::optional<addr_t> Node::get_vdf_reward_winner')
setup = section(validate, 'void Node::execution_context_t::setup_wait', '\nstd::shared_ptr<Node::execution_context_t> Node::new_exec_context')
source = '#include <mmx/Block.hxx>\n#include <mmx/ProofOfSpaceOG.hxx>\n#include <mmx/ProofOfSpaceNFT.hxx>\n' + foundation
source += '\nnamespace mmx {\n' + setup.replace('Node::', 'ReferenceNode::') + '\n}\n'
vdf_winner = section(node, 'vnx::optional<addr_t> Node::get_vdf_reward_winner', '\nstd::shared_ptr<const BlockHeader> Node::read_block')
blocks = (root / 'tools/reference_blocks.cpp').read_text().split('int main(')[0]
source += blocks.replace('// MMX_BLOCK_METHODS', methods.replace('Node::', 'BlockReference::')).replace('// MMX_VDF_METHOD', vdf_winner.replace('Node::', 'VdfReference::')).replace('VdfReference', 'PayoutReference')
admission = section(verify, 'void Node::verify_vdf(', '\nvoid Node::verify_vdf_cpu')
admission = admission[:admission.index('\tvdf_threads->add_task')] + '}\n'
work = admission + section(verify, 'void Node::verify_vdf_cpu', '\nvoid Node::verify_vdf_success')
work += section(node, 'std::shared_ptr<const BlockHeader> Node::find_prev(\tstd::shared_ptr<const BlockHeader>', '\nbool Node::find_challenge(const uint32_t vdf_height')
work += section(node, 'hash_t Node::get_challenge(std::shared_ptr<const BlockHeader>', '\nstd::pair<uint32_t, hash_t> Node::get_vdf_peak_ex')
work += section(node, 'std::vector<std::shared_ptr<const VDF_Point>>\nNode::find_vdf_points(', '\nstd::vector<std::shared_ptr<const VDF_Point>> Node::find_next_vdf_points(')
work += section(verify, 'void Node::verify_proof(std::shared_ptr<const BlockHeader>', '\nvoid Node::verify_vdf(')
work += section(validate, 'void Node::validate_diff_adjust', '\n\n} // mmx')
vdf = (root / 'tools/reference_vdf.cpp').read_text().split('using namespace mmx;')[0]
source += vdf.replace('// MMX_VDF_METHODS', work.replace('Node::', 'VdfReference::')).replace('VdfReference', 'WorkHistoryReference')
genesis = section(node, 'void Node::init_chain()', '\nvoid Node::trigger_update()')
# Substitute only the asset file reader; all genesis construction/checks remain.
genesis = genesis.replace('vnx::read_from_file<Transaction>', 'read_transaction').replace('Node::', 'GenesisReference::')
source += (root / 'tools/reference_chain.cpp').read_text().replace('// MMX_GENESIS_METHOD', genesis)
with tempfile.TemporaryDirectory(prefix='mmx-chain-') as tmp:
    cpp = pathlib.Path(tmp) / 'reference_chain.cpp'
    cpp.write_text(source)
    binary = pathlib.Path(tmp) / 'reference_chain'
    command = [link[0], '-std=c++17', '-O0', *flags['CXX_DEFINES'], *flags['CXX_INCLUDES'], str(cpp), '../libmmx_vm.a', '../libmmx_db.a']
    i = 1
    while i < len(link):
        arg = link[i]
        if arg == '-o':
            i += 2
            continue
        if not arg.endswith('test_pos_compute.cpp.o'):
            command.append(arg)
        i += 1
    command.extend(['-lgomp', '-o', str(binary)])
    subprocess.run(command, cwd=build / 'test', check=True)
    output = pathlib.Path(tmp) / 'chain.jsonl'
    subprocess.run([str(binary), str(output), str(a.source.resolve()), str(root / 'crates/mmx-consensus/tests/fixtures/work.jsonl')], cwd=a.source, check=True)
    (root / 'crates/mmx-execution/tests/fixtures/chain.jsonl').write_bytes(output.read_bytes())
