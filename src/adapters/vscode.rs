//! Adapter for the VS Code / Insiders Source (`workspaceStorage`).
//!
//! Three storage variants, including a delta-encoded JSONL mutation journal
//! (kind 0/1/2 replay). The Capture is the raw journal; replay happens at
//! extract time so replay bugs are fixable retroactively. Implementation
//! lands with slice 7.
