# HTTP 507 pauses the whole Digest run

Status: accepted

A local OpenAI-compatible server answers `POST /v1/chat/completions` with HTTP 507 (Insufficient Storage) when it cannot load the model under its memory ceiling. Treated like any other request error, every item of a nightly run failed within a minute, each logged separately, while the condition was the same for all of them and would not change within seconds.

A 507 is therefore a server-wide Capacity Wait rather than an item failure. At the first 507 the run stops sending: one worker retries the same request once per `capacity-retry-interval` (default `5m`) and the other workers block until the wait resolves. A non-507 answer ends the wait and the run continues as if nothing happened. After `capacity-max-wait` (default `75m`) the run ends with a single error naming the wait and the number of unsent items, and exits nonzero so the scheduler reports it. Unsent items are not counted as failed and leave no Rejected Digest; their selection is unchanged, so the next run picks them up. Pending Session title markers already make an interrupted title refresh durable.

The interval is fixed rather than exponential: memory is freed by other processes on their own schedule, and a steady probe bounds both the number of requests and the reaction time. Only 507 is classified this way. Other statuses (for example 503 from an overloaded proxy) keep the per-request failure, because they are not known to describe the whole server's ability to run the model.

## Consequences

- An unavailable model costs at most `capacity-max-wait` plus one request per interval, instead of a failure per item.
- A run may last up to `capacity-max-wait` longer than before; schedules must leave room for it.
- `digest-status.json` records the run as failed with the capacity message.
