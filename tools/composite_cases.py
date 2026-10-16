"""Shared command traces for the C++ oracle and Rust regression tests.

S/T/H/E/N denote static, stack, heap, external, and allocation-counter addresses.
Values: n/t/f, u:integer, r:address, a:size, m, s:hex, b:hex.
"""

SCENARIOS = """
[stack]
new 0
init
write T u:1337
commit
durable 1
new 1
init
read T

[heap_pinned]
new 0
init
alloc a
write @a u:1337
write S r:@a
commit
durable 1
new 1
init
read S
read @a
new 0
init
erase S
commit
durable 2
new 1
init
read S
read @a

[heap_replace]
new 0
init
alloc a
write @a u:1337
write S r:@a
alloc b
write @b u:11
write S r:@b
read @a
erase S
read @b
commit
durable 1
new 1
init
read S
read @a
read @b

[recursive_erase]
new 0
init
alloc a
alloc b
write @a m
write @b m
write S r:@a
write_key @a u:1337 r:@b
write_key @b u:1337 u:1337
read_key @b u:1337
erase S
read @a
read @b
read_key @b u:1337
commit
durable 1
new 1
init
read S
read @a
read @b

[lookup]
new 0
init
lookup 0 s:74657374
write S m
write_key S s:74657374 u:1337
commit
durable 1
new 1
init
lookup 1 s:74657374
read_key S s:74657374

[over_write]
new 0
init
write S u:1337
commit
durable 1
new 0
init
write S u:11337
commit
durable 2
new 1
init
read S

[array]
new 0
init
write S a:0
push S u:1
commit
durable 1
new 0
init
push S s:74657374
commit
durable 2
new 1
init
read S
read_entry S 0
read_entry S 1
copy T S
read T
read_entry T 0
read_entry T 1
new 0
init
pop T S
read T
commit
durable 3
new 1
init
read S
read_entry S 0
read_entry S 1

[map]
new 0
init
write S m
write_key S u:1 t
write_key S b:00010203 b:00010203
copy T S
read_key T u:1
read_key T b:00010203
commit
durable 1
new 0
init
read_key S u:1
read_key S b:00010203
write_key S u:1 u:2
write_key S b:00010203 s:74657374
commit
durable 2
new 0
init
erase_key S u:1
erase_key S b:00010203
commit
durable 3
new 0
init
read_key S u:1
read_key S b:00010203
write_key S u:1 u:3
write_key S b:00010203 n
commit
durable 4
new 1
init
read_key S u:1
read_key S b:00010203

[delete_write]
new 0
init
write S u:1337
commit
durable 1
new 0
init
read S
erase S
commit
durable 2
new 0
init
read S
write S u:11337
commit
durable 3
new 1
init
read S

[delete_write_key]
new 0
init
write S m
write_key S u:1337 u:11
commit
durable 1
new 0
init
read_key S u:1337
erase_key S u:1337
commit
durable 2
new 0
init
read_key S u:1337
write_key S u:1337 u:111
read_key S u:1337
commit
durable 3
new 1
init
read_key S u:1337

[aliases]
new 0
init
alloc a
alloc b
write @a a:0
push @a u:7
write @b m
write_key @b s:6b r:@a
write S r:@b
copy S+1 S
erase S
read @b
erase S+1
read @a
read @b
commit

[clone]
new 0
write 1 a:0
write 2 u:7
write 3 u:0
init
run CLONE,0,T+1,1;PUSH_BACK,1,T+1,2;GET,2,T,T+1,3;SIZE,2,S,T+1;RET,0
read T
read S
read H+4096
commit

[array_concat]
new 0
write 1 a:0
write 2 u:7
init
run CLONE,0,T+1,1;PUSH_BACK,1,T+1,2;CONCAT,6,T+2,T+1,T+1;POP_BACK,2,T,T+2;RET,0
read T
read H+4096
read H+4097
commit

[map_opcodes]
new 0
write 1 m
write 2 s:6b
write 3 u:9
init
run CLONE,0,T+1,1;SET,1,T+1,2,3;GET,2,T,T+1,2;ERASE,1,T+1,2;GET,2,S,T+1,2;RET,0
read T
read S
commit

[nested_persistence]
new 0
init
alloc a
write @a a:0
push @a u:7
write S m
write_key S s:6e6573746564 r:@a
commit
durable 1
new 0
init
push @a u:8
commit
durable 2
revert 1
new 1
init
read_key S s:6e6573746564
read @a
read_entry @a 0
read_entry @a 1

[read_only_new]
new 1
init
alloc a
write @a a:0
push @a u:7
write S r:@a
read_entry @a 0
commit

[stored_map_copy_fails]
new 0
init
write S m
write_key S s:6b u:1
commit
durable 1
new 0
init
copy T S

[stored_array_erase_fails]
new 0
init
write S a:0
commit
durable 1
new 0
init
erase S

[stored_map_replace_fails]
new 0
init
write S m
commit
durable 1
new 0
init
write S u:3

[pop_empty]
new 0
init
write S a:0
pop T S

[push_full]
new 0
init
assign S a:4294967295
push S u:1

[nil_key]
new 0
init
write S m
write_key S n u:1

[soft_get]
new 0
write 1 a:0
write 2 u:0
init
run GET,0,T,1,2;RET,0
read T

[hard_get]
new 0
write 1 a:0
write 2 u:0
init
run GET,16,T,1,2;RET,0

[set_out_of_bounds]
new 0
write 1 a:0
write 2 u:0
init
run SET,0,1,2,2;RET,0

[get_uint256_index]
new 0
write 1 a:0
write 2 u:0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
init
run GET,0,T,1,2;RET,0
read T

[readonly_loaded_array]
new 0
init
write S a:0
commit
durable 1
new 1
init
push S u:1

[entry_cannot_hold_descriptor]
new 0
init
write S a:0
push S m

[dangling_reference]
new 0
init
write S r:H+4096
erase S
commit

[temporary_reference_commit_fails]
new 0
init
write T u:1
write S r:T
commit

[self_reference]
new 0
init
alloc a
write @a m
write_key @a u:1 r:@a
write S r:@a
erase S
read @a
commit

[allocation_limit]
new 0
init
write N u:18446744073709551615
alloc a

[constant_key_reuse]
new 0
write 1 s:6b
write 2 s:6b
write 3 u:1
init
lookup 0 s:6b
lookup 0 u:1
write S m
write_key S s:6b u:9
commit
durable 1
new 1
init
read_key S s:6b

[return_cleanup]
new 0
write 1 a:0
init
run CALL,0,3,8;COPY,0,T,T+8;RET,0;CLONE,0,T,1;COPY,0,T+1,T;RET,0
read T
commit

[concat_alias]
new 0
write 1 a:0
init
run CONCAT,0,1,1,1;RET,0

[scalar_get]
new 0
write 1 u:7
write 2 u:0
init
run GET,0,T,1,2;RET,0
read T

[scalar_hard_get]
new 0
write 1 u:7
write 2 u:0
init
run GET,16,T,1,2;RET,0
"""


def cases():
    name = None
    commands = []
    for line in SCENARIOS.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            if name is not None:
                yield name, commands
            name, commands = line[1:-1], []
        else:
            commands.append(line)
    if name is not None:
        yield name, commands
    for depth in [49, 50, 51, 100]:
        commands = ["new 0", "init", "alloc n0", "write @n0 m"]
        for i in range(1, depth):
            commands.extend([f"alloc n{i}", f"write @n{i} m", f"write_key @n{i-1} u:1 r:@n{i}"])
        commands.extend(["write S r:@n0", "erase S"])
        if depth <= 50:
            commands.extend(["read @n0", f"read @n{depth-1}", "commit"])
        yield f"limits_erase_depth{depth}", commands
    for size in [4096, 4097]:
        key = "s:" + "ab" * size
        commands = ["new 0", "init", "write S m", f"write_key S {key} u:1"]
        if size == 4096:
            commands.extend(["commit", "durable 1", "new 1", "init", f"read_key S {key}"])
        yield f"limits_key{size}", commands
    yield "stored_rhs_set", ["new 0", "init", "write S m", "write S+1 s:6b", "write S+2 u:1", "commit", "durable 1", "new 0", "init", "run SET,0,S,S+1,S+2;RET,0", "commit"]
    for token in ["r:S", "a:0", "m"]:
        yield "fail_" + token.split(":")[0], ["new 0", f"write 1 {token}", "init", "run FAIL,0,1,7"]


def gas_boundaries(rows):
    """Replay each successful prefix at the C++ cost minus one/exact/plus one.

    Include failures at intermediate reads and cleanup inside compound operations
    by also sweeping their remaining gas in 257-unit steps.
    """
    current = None
    prefix = []
    previous_gas = 0
    for fields in rows:
        name, command, result, gas, *_ = fields
        gas = int(gas)
        if name != current:
            current, prefix, previous_gas = name, [], 0
        cost = gas - previous_gas
        op = command.split()[0]
        if not name.startswith("limits_") and cost > 0 and op not in {"new", "init", "budget", "durable", "revert"}:
            budgets = {0, max(cost - 1, 0), cost, cost + 1}
            if op in {"run", "commit", "clone", "copy", "erase", "write_key"}:
                budgets.update(range(0, cost, 257))
            for budget in sorted(budgets):
                yield f"{name}_step{len(prefix)}_gas{budget}", [*prefix, f"budget {budget}", command]
        prefix.append(command)
        previous_gas = gas
