# Model-generated Digests

[Back to README](../README.md)

Collection and Markdown rendering do not require a model. Digests optionally add summaries, classifications and stable titles for closed Session Days. Original Captures remain the evidence; model output can be wrong.

## Configure a model

Add a `[digest]` table to the Archive's `config.toml`:

```toml
[digest]
base-url = "https://model.example.org/v1"
model = "example-model"
api-key-env = "MODEL_API_KEY"
tokenizer = "/path/to/matching-model-tokenizer"
max-input-tokens = 60000
```

Replace the endpoint, model and tokenizer path with your own configuration. The endpoint must support the Chat Completions structured-output contract. Provide credentials through the named environment variable, outside repository files.

The tokenizer directory must contain `tokenizer.json` and a string `chat_template` in `tokenizer_config.json`. Use the same model revision as the inference server. Missing or invalid tokenizer data fails before model requests; there is no approximate-token fallback.

Token counting includes the chat template, generation prefix and configured `chat_template_kwargs`. `max-input-tokens` limits the complete rendered input. Oversized Messages are shortened in the middle with an explicit marker, preserving both ends; short Messages remain whole. Titles and correction retries obey the same cap.

The default `tokenizer-prompt-format = "chat-template"` counts ordinary template input. For oMLX without grammar decoding, `"omlx"` also counts its appended JSON-schema instruction. Choose the format matching the server; unknown values fail configuration parsing.

## Plan, generate and render

```sh
agent-sessions digest --archive "$archive" --dry-run
agent-sessions digest --archive "$archive"
agent-sessions render --archive "$archive"
```

Review the dry-run before scheduling model work, especially after upgrades. It reports new Digests, redone Digests by reason, fingerprints that can be backfilled without model calls, input tokens and an estimated duration. `--json` provides the structured report.

Completed run history supplies the duration estimate. Without it, the report says `no rate history` and shows measured input tokens. Unknown future model outputs are not counted. Tokenizer settings and sizing do not change Digest freshness.

A warning appears when redone Session Days exceed new ones or the configured `--redo-warn-fraction` of existing Digests. Review the cause; the warning does not change exit status. Dry-run takes no writer lock and writes nothing.

## Progress and recovery

```sh
agent-sessions digest status --archive "$archive"
```

Digest and repair runs count planned work by Source, log progress at `progress-interval` (default `"5m"`), and maintain `digest-status.json`. The ETA uses recent throughput. Status inspection does not take the Archive writer lock and exits nonzero for a failed run or a process that died without completing.

Existing fresh Digests survive upgrades and index rebuilds. Use explicit repair/force commands only when regeneration is intended; inspect each command's `--help`. `retitle` changes Locked Titles and reports View renames for external link repair. `push-titles` is a separate explicit write to supported native Stores, not part of collection or rendering.
