240 native next-VDF policy cases generated against `mmx-node`
`ca6438b37c08007106d58efe4a990f6ecb276649`.

The generator extracts `Node::find_next_vdf_points` verbatim. Synthetic verified
point metadata supplies branching inputs, infusion/iteration mismatches, arrival
order, missing paths and wrapping end iterations. The harness supplies the same
constant infusion/difficulty history used by Rust's real history resolver.
CPU verification is covered separately; no public unverified-point constructor
is added for these policy tests.

Regenerate with `python3 tools/generate_next_vdf_fixtures.py --source ../mmx-node`.
