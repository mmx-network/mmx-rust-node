#!/usr/bin/env python3
"""Execute original C++ TimeLord methods with deterministic thread boundaries."""
import argparse,json,shlex,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--source',type=Path,default=Path.home()/'workspace/mmx-node')
a=p.parse_args();root=Path(__file__).resolve().parent.parent;build=(a.source/'build').resolve()
target=build/'test/CMakeFiles/test_pos_compute.dir'
flags={k:shlex.split(v) for line in (target/'flags.make').read_text().splitlines() if ' = ' in line for k,v in [line.split(' = ',1)]}
link=shlex.split((target/'link.txt').read_text())
native=(a.source/'src/TimeLord.cpp').read_text()
methods=native[native.index('void TimeLord::start_vdf('):native.index('void TimeLord::vdf_loop(')]
methods+=native[native.index('hash_t TimeLord::compute('):native.index('void TimeLord::print_info(')]
source=(root/'tools/reference_timelord.cpp').read_text().replace('// MMX_TIMELORD_METHODS',methods.replace('TimeLord::','Reference::'))
with tempfile.TemporaryDirectory(prefix='mmx-timelord-') as directory:
    directory=Path(directory);cpp=directory/'reference.cpp';cpp.write_text(source)
    command=[link[0],'-std=c++17','-O2',*flags['CXX_DEFINES'],*flags['CXX_INCLUDES'],str(cpp)]
    i=1
    while i<len(link):
        arg=link[i]
        if arg=='-o':i+=2;continue
        if not arg.endswith('test_pos_compute.cpp.o'):command.append(arg)
        i+=1
    binary=directory/'reference';command+=['-o',str(binary)]
    subprocess.run(command,cwd=build/'test',check=True)
    output=directory/'timelord.jsonl';subprocess.run([str(binary),str(output)],check=True)
    commit=subprocess.check_output(['git','-C',str(a.source),'rev-parse','HEAD'],text=True).strip()
    rows=[dict(json.loads(line),reference_commit=commit) for line in output.read_text().splitlines()]
    destination=root/'crates/mmx-timelord/tests/fixtures/timelord.jsonl';destination.parent.mkdir(parents=True,exist_ok=True)
    destination.write_text(''.join(json.dumps(row,sort_keys=True)+'\n' for row in rows))
    print(f'{len(rows)} native timelord scenarios from {commit}')
