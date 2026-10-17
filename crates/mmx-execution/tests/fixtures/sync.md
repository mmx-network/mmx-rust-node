Root policy oracle generated from `mmx-node` commit
`ca6438b37c08007106d58efe4a990f6ecb276649`.

`tools/generate_sync_fixtures.py` extracts the commit-policy body verbatim from
`src/Node_update.cpp`. A small C++ harness supplies root/peak headers, proof
counts, sync position and pending heights and captures every committed height.
The 262 cases cover proof density, delay thresholds, weak branches, sync guards,
and native integer narrowing/wrapping. Native division by zero is excluded;
the Rust API rejects it explicitly.

Regenerate with `python3 tools/generate_sync_fixtures.py --source ../mmx-node`.
