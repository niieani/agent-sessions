# Technical design

## Codex rollover segments

Enumerate physical `rollout-*.jsonl` files into groups keyed by logical thread ID. Filename parser accepts:

- `rollout-<timestamp>-<thread-id>.jsonl`
- `rollout-<timestamp>-<thread-id>_<segment-id>.jsonl`

The first UUID remains logical identity. Sort segments by filename timestamp/path; concatenate their JSONL bytes into one compressed Capture, preserving each line and segment order. Do not deduplicate by ordinal: rollover ordinals may overlap.

Represent physical inputs explicitly in `SessionRef` and `CaptureMeta`. Each segment records path, mtime, size. Aggregate mtime is max; aggregate size is sum. Exact per-segment equality drives no-op gating and growth re-stat. Legacy single-file sidecars deserialize as one implicit segment.

Older adapters used the entire suffixed stem as identity. After the canonical aggregate is committed, move those generated bodies/sidecars under `retired-captures/` (body first, authoritative sidecar last), record their IDs monotonically in `supersededNativeIds`, and remove/skip their derived projections during pull, bundle ingest, and rebuild. This is recoverable reconciliation, not native-store mutation.

Goal attachment discovery scans the aggregate Capture after write, unchanged.

## Realtime delegation

Recognize only a sole leading `<realtime_delegation>` content part. Parse with `quick-xml` 0.42 into a closed direct-child schema; retain its required `<input>`, discard optional source/transcript fields, and reject nested, duplicate, unknown, mixed-block, or malformed envelopes. XML entity decoding follows the parser.

## Cursor NULL bubbles

Check `CaptureLine.value_type == "null"` before decoding/parsing. Skip. Empty text/blob values remain malformed and retain diagnostics because they claim to be JSON-bearing values.

## User instructions

Treat `<user_instructions>` as a known injected Codex envelope. Drop it without the unknown-tag warning; extracted dialog behavior is unchanged.

## Verification

Red integration tests first. Focused adapter tests during implementation. Then `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and full `cargo test`.
