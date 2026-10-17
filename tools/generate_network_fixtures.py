#!/usr/bin/env python3
"""Copy original bootstrap bytes and query native VNX/MMX text conversions."""
import argparse
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--source', type=Path, default=Path.home() / 'workspace/mmx-node')
p.add_argument('--build', type=Path)
a = p.parse_args()
root = Path(__file__).resolve().parent.parent
source = a.source.resolve()
build = (a.build or source / 'build').resolve()
fixtures = root / 'crates/mmx-execution/tests/fixtures'
assets = fixtures / 'network'
tracked = subprocess.run(['git', 'ls-files', '-z', 'config'], cwd=source, check=True, capture_output=True).stdout.decode().split('\0')
def copy_config(prefix):
    for name in tracked:
        if name.startswith(prefix + '/'):
            target = assets / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source / name, target)

for network in ('mainnet', 'testnet13'):
    copy_config(f'config/{network}/chain')
    shutil.copyfile(source / 'config' / network / 'parent', assets / 'config' / network / 'parent')
    directory = assets / 'data' / network
    directory.mkdir(parents=True, exist_ok=True)
    for file in (source / 'data' / network).iterdir():
        if file.suffix == '.dat' or file.name == 'tx_testnet_rewards.json':
            shutil.copyfile(file, directory / file.name)
copy_config('config/default')
# Exercise native inheritance and the directory-before-JSON override order.
layered = assets / 'config/layered'
(layered / 'chain/params').mkdir(parents=True, exist_ok=True)
(layered / 'parent').write_text('../testnet13/')
(layered / 'chain/params/port').write_text('20000')
(layered / 'chain/params.json').write_text('{port:30000,offer_binary:MMX,project_ratio:{value:7},"min_fee_ratio+":[13]}')
overlay = assets / 'config/overlay'
(overlay / 'chain/params').mkdir(parents=True, exist_ok=True)
(overlay / 'parent').write_text('../layered/')
(overlay / 'chain/params/port').write_text('40000')
(overlay / 'chain/params/min_fee_ratio+').write_text('[17]')

flags = {}
target = build / 'test/CMakeFiles/test_pos_compute.dir'
for line in (target / 'flags.make').read_text().splitlines():
    if ' = ' in line:
        key, value = line.split(' = ', 1)
        flags[key] = shlex.split(value)
link = shlex.split((target / 'link.txt').read_text())
cases = [('params', 'defaults', '{}')]
for field in ('port', 'decimals', 'initial_time_diff', 'block_interval_ms'):
    for value in (None, True, False, -1, 0, 4294967297, 18446744073709551615, '1234', 'garbage', [], {}):
        cases.append(('params', f'{field}_{value}', json.dumps({field: value})))
for value in (None, 'MMX', 'broken', [1, 257, -1], 513):
    cases.append(('params', f'address_{value}', json.dumps({'offer_binary': value})))
cases.extend([
    ('params', 'unknown', '{unknown:99,template_binary:broken,project_ratio:{value:7},min_fee_ratio:3}'),
    ('params', 'null_fee_vector', '{min_fee_ratio:null}'),
])
for field in ('address', 'contract', 'amount'):
    for value in (None, True, -1, 513, 18446744073709551615, [], [1, 257, -1], 'MMX', 'garbage', '123', '0x1234', str(2**128-1), str(2**128), str(2**256+17), '1!2', '12\x0034'):
        cases.append(('rewards', f'{field}_{value}', json.dumps({'__type': 'mmx.Transaction', 'outputs': [{field: value}]})))
for value in ('REWARD', 'unknown', '123', 4294967297, None):
    cases.append(('rewards', f'note_{value}', json.dumps({'__type': 'mmx.Transaction', 'note': value})))
cases.extend([
    ('rewards', 'fields', '{__type:mmx.Transaction,id:123,content_hash:broken,sender:MMX,network:mainnet,nonce:-1,outputs:{amount:"17",memo:"hello"},exec_result:{did_fail:true,inputs:[{address:513,amount:99,flags:257}],error:{code:3,line:7,message:"oops"}}}'),
    ('rewards', 'hashes', '{__type:mmx.Transaction,id:"010203040506070809000102030405060708090001020304050607080900010203",content_hash:"0x010203040506070809000102030405060708090001020304050607080900010203"}'),
])
with tempfile.TemporaryDirectory(prefix='mmx-network-') as tmp:
    tmp = Path(tmp)
    binary = tmp / 'reference_network'
    command = [link[0], '-std=c++17', '-O0', *flags['CXX_DEFINES'], *flags['CXX_INCLUDES'], str(root / 'tools/reference_network.cpp')]
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
    rows = []
    for network in ('mainnet', 'testnet13', 'layered', 'overlay'):
        run = subprocess.run([str(binary), 'config', str(assets / 'config' / network)], check=True, capture_output=True, text=True)
        rows.append(dict(kind='config', name=network, **json.loads(run.stdout)))
    for kind, name, text in cases:
        path = tmp / 'input.json'
        path.write_text(text)
        run = subprocess.run([str(binary), kind, str(path)], check=True, capture_output=True, text=True)
        rows.append(dict(kind=kind, name=name, input=text, **json.loads(run.stdout)))
    (fixtures / 'network.jsonl').write_text(''.join(json.dumps(row, separators=(',', ':')) + '\n' for row in rows))
    print(f'{len(rows)} native network/text cases')
