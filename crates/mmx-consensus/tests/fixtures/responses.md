Native `ProofResponse` oracle from `mmx-node` commit
`ca6438b37c08007106d58efe4a990f6ecb276649`.

The harness links native hash/static-validation/signature methods. It wraps five
real signed branch fixtures and tests 16 mutations for each: bad hashes/content,
invalid/zero/wrong-key signatures, absent proofs, invalid plot IDs, proof shape,
VDF-height boundaries, 1023/1024-byte harvester strings and unhashed local fields
including non-UTF8/NUL bytes. Expected error strings are retained.

Regenerate with `python3 tools/generate_response_fixtures.py --source ../mmx-node`.
