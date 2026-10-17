#!/usr/bin/env python3
"""Compile the actual native Node validation/execution methods against a test store."""
import argparse
import pathlib
import shlex
import subprocess
import tempfile
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=pathlib.Path,default=pathlib.Path.home()/'workspace/mmx-node')
p.add_argument('--build',type=pathlib.Path)
a=p.parse_args()
root=pathlib.Path(__file__).resolve().parent.parent
build=(a.build or a.source/'build').resolve()
target=build/'test/CMakeFiles/vm_engine_tests.dir'
flags={}
for line in (target/'flags.make').read_text().splitlines():
    if ' = ' in line:
        key,value=line.split(' = ',1)
        flags[key]=shlex.split(value)
link=shlex.split((target/'link.txt').read_text())
native=(a.source/'src/Node_validate.cpp').read_text()
start=native.index('void Node::execute(\tstd::shared_ptr<const Transaction>')
end=native.index('\nvoid Node::validate_diff_adjust',start)
methods=native[start:end].replace('Node::','ReferenceNode::')
source=(root/'tools/reference_transactions.cpp').read_text().replace('// MMX_NATIVE_VALIDATION',methods)
with tempfile.TemporaryDirectory(prefix='mmx-transactions-') as tmp:
    cpp=pathlib.Path(tmp)/'reference_transactions.cpp'
    cpp.write_text(source)
    binary=pathlib.Path(tmp)/'reference_transactions'
    command=[link[0],'-std=c++17','-O0',*flags['CXX_DEFINES'],*flags['CXX_INCLUDES'],str(cpp)]
    i=1
    while i<len(link):
        arg=link[i]
        if arg=='-o':
            i+=2
            continue
        if not arg.endswith('engine_tests.cpp.o'):
            command.append(arg)
        i+=1
    command.extend(['-o',str(binary)])
    subprocess.run(command,cwd=build/'test',check=True)
    out=root/'crates/mmx-execution/tests/fixtures/transactions.jsonl'
    subprocess.run([str(binary),str(out)],check=True)
