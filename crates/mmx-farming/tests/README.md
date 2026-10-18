`fixtures.json` selects the positive CPU compute witness and first positive
OG proof from `mmx-consensus/tests/fixtures/work.jsonl`. See that directory's
`work.md` for the exact C++ revision and generator. The final Y, 48-byte metadata,
canonical 256-X proof, proof identity, challenge, parameters and acceptance are
C++ outputs. Tests pack that witness into tiny native HDD/SSD-style park files,
then read it through the real VNX PlotHeader and positional park reader. C1 uses
the same witness with one low bit omitted from X and recomputed on the CPU.
These synthetic k8 files exercise the format without allocating production k29+
plots. No real wallet or farmer keys, plots, node database, or network is used.

The farmer secret vector uses seed bytes 1..32 and an independent Python
`hmac.digest` / `hashlib.sha256` transcription of `hmac_sha512.hpp` and
`ECDSA_Wallet::create_farmer_key`; signatures are checked by the existing native
consensus verifier. Native authorization fixture tests independently cover ECDSA
compatibility. RPC tests open ephemeral loopback ports and disposable accounts.

`native/` additionally contains C++-written plot files, reopened and fully proven
by the original `pos::Prover` (HDD, SSD and C1), and the C++ wallet farmer key.
Regenerate using `nice -n 19 ionice -c 3 python3 tools/generate_farming_fixtures.py`.
It compiles only the small harness against existing libraries; it never builds
the C++ project or opens ledger/wallet storage. The fixture seed is public test
data (bytes 1..32). No CUDA initialization or external service is performed.
