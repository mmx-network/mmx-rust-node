# Native production proof fixture

Generated against C++ `mmx-node` revision
`5626ac2b7cd26aa9626ea3b2cf864b4cfe6fbcd2` with
`tools/generate_production_fixtures.py`. The harness links the existing C++
libraries and copies `Node::verify_proof_impl` / `Node::verify_proof` verbatim.
It never opens node or wallet storage or initializes a GPU.

The fixture contains a stronger proof for the first synthetic chain challenge,
owned by public test scalar 2. The weaker proof uses scalar 1 and comes from the
existing native chain fixture. Both have canonical 256-X proofs verified by C++.
A bounded copy of `compute_full` is used only to search for a tiny k8 witness;
the original linked verifier checks the final result.

Socket tests use these competing proofs to check global proof ranking before
full-block construction, a better proof arriving during signing, sorted inclusion
of local and remote proofs, verified announcement relay, metadata stripping and
echo suppression. Further cases cover native voting scope and failed-attempt
retry without signing an already created proof again.
