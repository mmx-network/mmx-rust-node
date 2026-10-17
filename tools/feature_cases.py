"""Native traces for conversions, crypto, transfers and node callbacks."""


def string(value):
    return 's:' + value.hex()


def binary(value):
    return 'b:' + value.hex()


def address_text(raw, hrp='mmx', padding=0):
    charset = 'qpzry9x8gf2tvdw0s3jn54khce6mua7l'
    bits = int.from_bytes(raw, 'little')
    words = [(bits >> (251 - i * 5)) & 31 for i in range(51)] + [((bits & 1) << 4) | padding]
    values = [ord(c) >> 5 for c in hrp] + [0] + [ord(c) & 31 for c in hrp] + words + [0] * 6
    chk = 1
    for v in values:
        top = chk >> 25
        chk = ((chk & 0x1ffffff) << 5) ^ v
        for i, g in enumerate([0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3]):
            if (top >> i) & 1:
                chk ^= g
    chk ^= 0x2bc830a3
    return (hrp + '1' + ''.join(charset[v] for v in words + [(chk >> (5 * i)) & 31 for i in reversed(range(6))])).encode()


def cases():
    values = ['n', 'f', 't', 'u:0', 'u:1', 'u:4294967296', 'u:18446744073709551616', 'u:' + str(2**128), 'u:' + str(2**256-1), 's:', string(b'0x123AbF'), string(b'123456789'), string(b'0b101'), string(b'-1'), string(b'\xff\x00'), 'b:', binary(bytes(range(32))), 'b:ffaa', 'r:2', 'a:0', 'm']
    for i, value in enumerate(values):
        for kind in range(14):
            yield f'matrix_conv_{i}_{kind}', ['new 0', f'write 1 {value}', 'write 2 u:7', 'init', f'run CONV,0,S,1,{kind},0;RET,0', 'read S']
    for i, value in enumerate(values):
        for fmt in [0, 1, 2, 3, 4, 5, 10, 11, 12, 13, 255]:
            for kind in [6, 7, 8, 10]:
                yield f'matrix_format_{i}_{fmt}_{kind}', ['new 0', f'write 1 {value}', 'init', f'run CONV,0,S,1,{kind + (fmt << 8)},{fmt};RET,0', 'read S']
    texts = [b'', b'0x', b'0b', b'00', b'0XFF', b' 10', b'+1', b'1\x00', b'ABCDEF', b'09', b'0x10', b'0b11', b'1' * 257, b'f' * 65, b'7' * 100]
    for i, text in enumerate(texts):
        for flag in range(6):
            for kind in [6, 8]:
                yield f'matrix_parse_{i}_{flag}_{kind}', ['new 0', f'write 1 {string(text)}', 'init', f'run CONV,0,S,1,{kind},{flag};RET,0', 'read S']
    for i, raw in enumerate([bytes(32), bytes(range(32)), bytes([255])*32, bytes([1]) + bytes(31)]):
        text = address_text(raw)
        for j, variant in enumerate([text, text.upper(), address_text(raw, 'other'), address_text(raw, padding=15), b'MMX', b'mmx', text[:-1] + b'!', text[:5].upper() + text[5:], b'a'*91]):
            yield f'matrix_address_{i}_{j}', ['new 0', f'write 1 {string(variant)}', 'init', 'run CONV,0,S,1,10,0;RET,0', 'read S']
        yield f'address_roundtrip_{i}', ['new 0', f'write 1 {binary(raw)}', 'init', 'run CONV,0,S,1,2567,0;CONV,0,S+1,S,10,0;RET,0', 'read S', 'read S+1']
    yield 'conversion_ref_flags', ['new 0','write 1 u:6','write 2 u:0','write T+1 r:3','write 3 s:313233','init','run CONV,14,S,T+1,1,2;RET,0','read S']
    yield 'conversion_high_flag_bits', ['new 0','write 1 s:313233','write 2 u:4294967302','write 3 u:4294967296','init','run CONV,12,S,1,2,3;RET,0','read S']
    for i, number in enumerate([0, 2**32-1, 2**32, 2**64-1, 2**64, 2**128-1, 2**128, 2**256-1]):
        yield f'conv_cost_{i}', ['new 0', f'write 1 u:{number}', 'init', 'run CONV,0,S,1,1031,0;RET,0', 'read S']
    for i, variant in enumerate(['', 'write 1 b:' + '00'*32, 'write 2 b:' + '00'*33, 'write 3 b:' + '00'*64, 'write 3 b:' + 'ff'*64, 'write 3 b:00', 'write 2 b:00', 'write 1 b:00', 'write 1 s:00']):
        commands = ['new 0', 'signature']
        if variant: commands.append(variant)
        yield f'verify_{i}', [*commands, 'init', 'run VERIFY,0,S,1,2,3;RET,0', 'read S']
    yield 'verify_zero_cache_entry', ['new 0','write 1 b:'+'00'*32,'write 2 b:'+'00'*33,'write 3 b:'+'00'*64,'init','run VERIFY,0,S,1,2,3;RET,0','read S']
    yield 'verify_storage_order', ['new 0','signature','copy S+1 1','copy S+2 2','copy S+3 3','init','commit','durable 1','new 0','init','run VERIFY,0,T,S+1,S+2,S+3;RET,0','read T']
    currency = binary(bytes(32))
    dest = binary(bytes(range(32)))
    setup = ['new 0', f'balance {currency} 1000', f'write 1 {dest}', 'write 2 u:7', f'write 3 {currency}', 'write 4 n', 'init']
    yield 'transfers', [*setup,'run BALANCE,0,S,3;SEND,0,1,2,3,4;BALANCE,0,S+1,3;SEND,0,1,2,3,4;MINT,0,1,2,4;BALANCE,0,S+2,3;RET,0','read S','read S+1','read S+2','effects','commit']
    for i, memo in enumerate(['n','s:',string(b'\xff'*64),string(b'a'*65),'u:1','b:00']):
        for opcode in ['SEND,0,1,2,3,4', 'MINT,0,1,2,4']:
            yield f'transfer_memo_{i}_{opcode[:4]}', [*setup[:-1],f'write 4 {memo}','init',f'run {opcode};RET,0'] + (['effects'] if i<3 else [])
    for i, amount in enumerate([0, 1, 1000, 1001, 2**80-1, 2**80, 2**128-1, 2**128, 2**256-1]):
        for opcode in ['SEND,0,1,2,3,4','MINT,0,1,2,4']:
            yield f'transfer_amount_{i}_{opcode[:4]}', [*setup[:-1],f'write 2 u:{amount}','init',f'run {opcode};RET,0']
    yield 'transfer_zero_skip', ['new 0','write 1 u:0','init','run SEND,0,0,1,0,0;MINT,0,0,1,0;RET,0','effects']
    yield 'transfer_null_currency', ['new 0',f'write 0 {currency}','write 1 u:1','init','run SEND,0,0,1,0,0;RET,0']
    yield 'balance_missing', ['new 0',f'write 1 {currency}','init','run BALANCE,0,S,1;BALANCE,0,S+1,1;RET,0','read S','read S+1']
    for op in ['LOG,0,4,1','EVENT,0,1,2','RCALL,0,1,2,5,1','CREAD,0,S,1,2']:
        for enabled in [0,1]:
            yield f'hook_missing_{op[:3]}_{enabled}', ['new 0',f'hooks {enabled}','init',f'run {op};RET,0']
    yield 'logs_events', ['new 0','hooks 1','write 1 s:ff00','write 2 u:123','write 3 u:4294967303','init','run LOG,1,3,1;EVENT,0,1,2;RET,0','effects']
    yield 'remote_call', ['new 0','hooks 1','write 1 s:646570','write 2 s:6563686f','write T+6 u:123','write T+7 u:456','init','run RCALL,0,1,2,5,1;COPY,0,S,T+5;RET,0','read S','read T+6','effects','commit']
    yield 'remote_call_array', ['new 0','hooks 1','write 1 s:646570','write 2 s:6563686f','write T+6 a:0','push T+6 u:123','init','run RCALL,0,1,2,5,1;COPY,0,S,T+5;RET,0','read S','read_entry S 0','effects','commit']
    yield 'remote_call_zero_args', ['new 0','hooks 1','write 1 s:646570','write 2 s:6563686f','init','run RCALL,0,1,2,0,0;RET,0','read T','effects']
    yield 'remote_call_host_failure', ['new 0','hooks 1','write 1 s:646570','write 2 s:6661696c','init','run RCALL,0,1,2,0,0;RET,0']
    for offset,nargs in [(2**30,1),(0,4097),(2**30-1,4096)]:
        yield f'remote_limits_{offset}_{nargs}', ['new 0','hooks 1','write 1 s:646570','write 2 s:6563686f','init',f'run RCALL,0,1,2,{offset},{nargs};RET,0']
    yield 'contract_read', ['new 0','hooks 1',f'write 1 {dest}','write 2 s:6669656c64','init','run CREAD,0,S,1,2;RET,0','read S','effects','commit']
    yield 'contract_read_storage_order', ['new 0',f'write S+1 {dest}','write S+2 s:6669656c64','init','commit','durable 1','new 0','hooks 1','init','run CREAD,0,T,S+1,S+2;RET,0','read T','effects']
    yield 'remote_storage_order', ['new 0','write S+1 s:646570','write S+2 s:6563686f','init','commit','durable 1','new 0','hooks 1','init','run RCALL,0,S+1,S+2,0,0;RET,0','read T','effects']
    # Compact ECDSA scalar boundaries and high-S rejection, seeded by the
    # native deterministic signature for private key 1 / message 0x2a00... .
    order = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141
    r = 0x3979eb2ef6d03ad7bc0c2ad4f0237670333d35df4f701dfb9087cd90f9f6157e
    s = 0x4731bfb9ed30faa7603ff2e4cd1cf7fbf9b12e5d13ce7a0c94df6bae152dfae3
    for i, (rv, sv) in enumerate([(r, order-s), (0,s), (r,0), (order,s), (r,order), (order-1,order-1), (1,1)]):
        sig = (rv.to_bytes(32,'big') + sv.to_bytes(32,'big')).hex()
        yield f'verify_scalar_{i}', ['new 0','signature',f'write 3 b:{sig}','init','run VERIFY,0,S,1,2,3;RET,0','read S']
    yield 'conv_operand_order', ['new 0','write S+1 r:S+11','write S+2 r:S+12','write S+3 u:6',f'write S+4 {string(b"x"*40)}','init','commit','durable 1','new 0','init','run CONV,15,S+1,S+2,S+3,S+4;RET,0']
    yield 'log_operand_order', ['new 0','write S+1 s:01','write S+2 r:S+12','init','commit','durable 1','new 0','hooks 1','init','run LOG,3,S+1,S+2;RET,0']

    for opcode, nargs in [('COPY',2),('CLONE',2),('GET',3),('SET',3),('ERASE',2),('PUSH_BACK',2),('POP_BACK',2),('CONCAT',3),('MEMCPY',4),('SHA256',2)]:
        args = ','.join(f'S+{i+1}' for i in range(nargs))
        yield f'indirect_order_{opcode}', ['new 0', f'write S+2 {binary(bytes(40))}', 'write S+3 u:1', 'write S+4 u:0', 'init','commit','durable 1','new 0','init',f'run {opcode},{(1<<nargs)-1},{args};RET,0']

    yield 'full_width_send', ['new 0', f'balance {currency} {2**128-1}', f'write 1 {dest}', f'write 2 u:{2**128-1}', f'write 3 {currency}', 'write 4 n', 'init', 'run SEND,0,1,2,3,4;BALANCE,0,S,3;RET,0', 'read S', 'effects']
    yield 'remote_local_call_frame', ['new 0', 'hooks 1', 'write 1 s:646570', 'write 2 s:6563686f', 'write T+16 u:123', 'init', 'run CALL,0,2,10;RET,0;RCALL,0,1,2,5,1;COPY,0,S,T+5;RET,0', 'read S','read T+16','effects']
    for opcode in ['SEND,0,1,2,3,4', 'MINT,0,1,2,4']:
        for cell, value in [(1,'n'),(1,'b:00'),(2,'n'),(2,'b:00'),(3,'n'),(3,'b:00')]:
            if opcode.startswith('MINT') and cell == 3:
                continue
            yield f'transfer_invalid_{opcode[:4]}_{cell}_{value[0]}', [*setup[:-1],f'write {cell} {value}','init',f'run {opcode};RET,0']
