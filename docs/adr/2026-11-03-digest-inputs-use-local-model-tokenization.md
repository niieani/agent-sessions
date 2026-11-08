# Digest inputs use local model tokenization

Status: accepted. Release: 0.4.11.

A numeric CSV paste measured about 119k model tokens while bytes/4 predicted
44k, bypassing a 60k input limit. Day splitting cannot divide one Message.

Use Hugging Face tokenizers 0.23.2 (latest stable, maintained upstream), with
default features disabled and fancy-regex enabled: no Oniguruma C dependency.
The 1.0 release candidate rewrites the inference API; retain the current stable
API for this release. No unrelated dependency upgrades.

`digest.tokenizer` explicitly points to the configured model's local HF directory.
Render its chat template with the existing MiniJinja engine, model metadata,
add_generation_prompt, and chat_template_kwargs. HTTP and counting share one
Message construction function, including correction turns. Unsupported template
operations fail explicitly; no approximate or network fallback. Provider prompt additions require an explicitly selected prompt format.

Upstream oMLX exposes /tokenize; installed 0.7.0 returns HTTP 404 for /v1/tokenize.
Local counting supports offline dry-runs and fast repeated sizing without server
or model-load dependencies. TokenCounter separates tokenization from budgeting;
unit tests inject a deterministic counter, integration tests use a tiny local
HF tokenizer fixture rather than downloading a model.

Middle-truncate the largest Message first, preserving both ends with a clear
marker. Repeat until the fully rendered request fits. Re-encoding after each
UTF-8-safe cut handles changed token boundaries. This replaces turn reduction
and the whole-body tail cut, preserving short replies and Session Day openings.
Titles and retries fail before sending an oversized request.

Neither PROMPT_VERSION, stored Digest schema, nor Dialog fingerprints change.
Existing fresh Digests remain fresh; no upgrade regeneration or backfill needed.
Linux PR CI builds the release binary and checks ELF INTERP and NEEDED entries.

For oMLX without grammar decoding, set `[digest] tokenizer-prompt-format = "omlx"`.
This counts its response_format JSON-schema instruction appended to the system
Message, using the actual schema for each request kind. The default
`"chat-template"` counts the template without provider schema injection. Select
the format matching the server; unknown formats fail configuration parsing.

Verification on the failing chatgpt:683903c9 Session Day: 119,241 full input
tokens, exactly matching oMLX; 51,323 after middle truncation, including its
schema instruction. Ten release counts average 44 ms; prompt fitting takes
238 ms. Both ends of the 173,913-character paste and the complete 3,982-character
reply survive. Measurements are machine-specific; the ignored opt-in test
`measure_local_prompt` accepts DIGEST_CHECK_ARCHIVE pointing to a disposable
archive clone for reproduction without a model call.
