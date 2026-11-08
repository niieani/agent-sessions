# Verification

- Codex rollover regression: fresh grouping, missing newline boundary, bookmarked v2 upgrade, legacy Capture quarantine, both-segment growth, durable tombstone, rebuild equivalence.
- Realtime regression: numeric entity decode; nested, duplicate, trailing-root, and mixed-block rejection.
- Cursor regression: real SQLite NULL row emits no bad-JSON warning; malformed BLOB remains diagnosed.
- Full `cargo test`: pass.
- `cargo clippy --all-targets --all-features -- -D warnings`: pass.
- `cargo fmt --check`; `git diff --check`: pass.
- Builder compliance and cleanup reviews: no remaining findings.
