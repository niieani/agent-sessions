Synthetic Codex rollout records, built from `digest-upgrade/dialog.json` plus
the redacted envelope shapes supplied in the 2026-11-07 bug report. No private
Archive data. Session ids intentionally match the released Digest fixtures.

`unchanged-*.jsonl` reproduce the released Dialog, adding only known machine
injections and question-reply UI/compaction mirrors. `restored.jsonl` additionally
contains one response-item user reply with two question/answer objects. Only
that response item may add Dialog or invalidate Digests. Multiline long answers,
malformed bodies, unknown tags and quoted machine tags have adapter unit tests.
