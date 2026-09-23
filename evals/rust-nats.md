# rust-nats

### Triggering

**Should load**

1. "Add a NATS publisher to the orders service: when POST /orders succeeds, publish an OrderPlaced event to JetStream so the billing worker can pick it up."
2. "The billing worker should read `orders.>` from JetStream, pick up where it left off after a restart and ack each message. How do I run it as a background task next to axum?"
3. "My worker stops consuming after a NATS restart. The logs show `consumer deleted` once and then nothing; the process is still up and /health/ready still says `\"messaging\": \"ok\"`."
4. "Request-reply to the pricing service returns `Error { kind: NoResponders, .. }` in staging but works locally. How should the handler map that, 502 or 503?"
5. "Write integration tests for our JetStream consumer with testcontainers — the same reuse-by-name pattern we use for Postgres, and make sure a duplicate Nats-Msg-Id is dropped."
6. "Messages that fail to parse get redelivered forever. Where should the bad ones go so we can look at them later?"
7. "Our NATS integration tests pass one at a time but fail at random when nextest runs them in parallel."
8. "Set up pub/sub between our Rust services so the catalog service hears about every price change."

**Should not load**

1. "Cache the GET /users/{id} response in Redis for 60 seconds with invalidation on update." -> `rust-redis`
2. "Add a `/health/ready` endpoint that checks the database pool." -> `axum-service`
3. "Write a sea-orm migration that adds a unique index on `processed_events.event_id`." -> `sea-orm-postgres`
4. "After signup, send the welcome email from a background task and retry it twice if the SMTP call fails." -> `axum-service`
5. "Our Postgres testcontainers fixture fails under nextest with `409 container name already in use`." -> `sea-orm-postgres`
6. "Stream order status updates to the browser with Server-Sent Events." -> `axum-service`
7. "Rate limit `POST /orders` to 10 requests a minute per user, backed by Redis." -> `rust-redis`
8. "Propagate `traceparent` from incoming requests to our outbound reqwest calls so both show up in one trace." -> `axum-service`

### Eval 1 - durable publish from a handler

**Prompt**: "Add JetStream publishing to POST /orders: an `OrderPlaced` event with the order id, deduplicated so a retried publish does not create two events. Map NATS failures onto AppError."

**Must produce**:
- No crate beyond `async-nats`, which the project already declares, added for NATS.
- `async_nats::Client` and `jetstream::Context` added to `AppState` by value, with no `Arc`.
- `jetstream::new(client)` without `.await`.
- `send_publish(subject, PublishMessage::build().payload(..).message_id(event_id))` with **two** awaits.
- The `Nats-Msg-Id` fixed per event (minted with the event or derived from the order id), not per publish attempt.
- A stream declared at startup with a `duplicate_window`.
- `DiscardPolicy::New` on the stream, or a note that the default `DiscardPolicy::Old` silently drops the oldest message at a limit.
- The stream declaration retried with backoff when it returns `TimedOut` (the client may still be connecting under `retry_on_initial_connect`).
- A `MessagingError` (or equivalent) with a `#[from]` arm for JetStream `PublishError`.
- The JetStream `PublishError` classified by `err.kind()`.
- A `fn messaging_error(MessagingError) -> AppError` applied at the call site with `.map_err(messaging_error)?`.
- `StreamNotFound`, `TimedOut`, `BrokenPipe` and `MaxAckPending` mapped to `AppError::Unavailable` (503).
- `MaxPayloadExceeded` mapped to `BadRequest` (400).
- The remaining JetStream kinds (`WrongLastSequence`, `WrongLastMessageId`, `Other`) mapped to `Other` (500).
- A subject of the form `orders.placed.<id>`.

**Must not produce**:
- A single await on the JetStream publish.
- `jetstream::context::Publish::build()` (deprecated since 0.44).
- A new `AppError::Messaging` variant.
- A `From<MessagingError> for AppError` impl, or any other edit to `error.rs`.
- A 502 or 504 for `TimedOut`.
- "A responder exists and is slow" given as the meaning of `TimedOut`.
- A new client per request.
- `Arc<async_nats::Client>` in state.
- `Nats-Msg-Id` described as making the consumer idempotent.
- The `nats` crate.

### Eval 2 - the billing worker

**Prompt**: "Write the billing worker that consumes `orders.placed.*` from the ORDERS stream. It must survive NATS restarts, not lose messages, stop cleanly on shutdown, and send poison messages somewhere we can inspect."

**Must produce**:
- A durable **pull** consumer via `get_or_create_consumer` (or `create_consumer`, described as create-or-update).
- The consumer config setting `AckPolicy::Explicit`, `ack_wait`, `max_deliver`, `filter_subject` and `max_ack_pending`.
- `consumer.messages()` for the long-lived loop, not `fetch()` in a `loop {}`.
- An outer loop that treats the first `Err` item from `messages()` as terminal for that stream: log, pause with backoff, rebuild the consumer, continue.
- The handler as a plain function returning an outcome, with the loop doing the acking.
- `double_ack()` on success.
- `ack_with(AckKind::Nak(Some(delay)))` for a transient failure.
- `ack`/`info` errors converted with `.map_err(|err| anyhow::anyhow!(err))` or returned as `async_nats::Error`, never `?` into `anyhow::Result`.
- The reopen backoff pause ending early on the stop signal (`timeout(backoff, stop.changed())` or a `select!`), not a bare `sleep`.
- Poison handled by publishing the original payload and headers to a `dlq.`-prefixed subject keeping the original (`dlq.<subject>` or `dlq.<consumer>.<subject>`) with a durable two-await publish, *then* `ack_with(AckKind::Term)`.
- A dead-letter stream capturing the `dlq.` subjects (`dlq.orders.>`, `dlq.billing.>` or similar) plus the `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.ORDERS.>` advisory (or its per-consumer form) for messages that exhausted `max_deliver`.

**Must not produce**:
- A push consumer or `deliver_subject`.
- An in-handler retry loop.
- `message.in_progress()` or `message.respond()`.
- `?` on an `Err` item that returns from the worker task, or `continue` past it.
- "`messages()` ends" or "the stream returns `None`" given as the only reason the outer loop exists.
- `delete_consumer` followed by create as the way to change the consumer's config.
- `tokio_util::sync::CancellationToken` presented as already in the stack without adding the dependency.
- `Term` alone presented as routing a poison message to the DLQ (it fires `MSG_TERMINATED`, which a max-deliveries listener never sees).
- `ack_wait` and `backoff` both set on the consumer without noting that `backoff[0]` replaces `ack_wait`.

### Eval 3 - flaky NATS tests under nextest

**Prompt**: "Our NATS integration tests are flaky under nextest: sometimes `409 container name already in use`, sometimes `expected INFO, got nothing`, and two tests occasionally see each other's messages. Fix the harness."

**Must produce**:
- A reused container (`with_container_name` + `ReuseDirective::Always`).
- `.with_tag("2.12-alpine")` on the NATS image.
- `NatsServerCmd::default().with_jetstream()` passed by reference through `with_cmd`.
- A retry loop around `start()`, with the nextest process-per-test race as the reason.
- A retry loop around `connect()` for the early-attacher race.
- `TEST_NATS_URL` short-circuiting the container.
- A per-test uuid prefix on every subject, stream and bucket.
- `delete_stream` at the end of each test.
- `tokio::time::timeout` around every wait on a subscription or consumer stream.
- A note that a `LEAK` status from nextest is a pass.

**Must not produce**:
- A `static`/`OnceLock` container presented as shared across tests under nextest.
- `FLUSH`-style server-wide cleanup.
- A fixed stream name shared by tests.
- The module's default tag (`2.10.14`) left in place.
- A mocked NATS client instead of a server.

### Eval 4 - request-reply over a queue group

**Prompt**: "Serve `pricing.quote` from a `pricing` queue group, replying with the price; the caller sends a sku and gives up after 500 ms. Handle the case where nothing is subscribed."

**Must produce**:
- `client.queue_subscribe("pricing.quote", "pricing".to_owned())` driven as a `Stream` with `while let Some(message) = requests.next().await`.
- Replies sent with `client.publish(reply, ..)` to `message.reply`, with the `None` case skipped (no `respond` in core NATS).
- `send_request("pricing.quote", Request::new().payload(..).timeout(Some(Duration::from_millis(500))))`.
- `err.kind() == RequestErrorKind::NoResponders` distinguished from `TimedOut`.
- Both mapped to `AppError::Unavailable` (503) through `messaging_error`.
- The note that `TimedOut` is also what a disconnected client returns, and is therefore not a 504.
- A mention that `ConnectOptions` needs `.retry_on_initial_connect()`, which async-nats leaves off by default.

**Must not produce**:
- `message.respond(..)` on a core `async_nats::Message`.
- A match on the error enum variants instead of `kind()`.
- A callback-style subscribe API.
- `no responders` described as arriving immediately regardless of connection state.

### Eval 5 - a feature-flag watcher

**Prompt**: "Replace our Redis feature flags with a NATS KV bucket: the service needs the current value of `checkout.v2` at startup and every change afterwards."

**Must produce**:
- `create_key_value` / `get_key_value` for the bucket.
- `watch_with_history("checkout.v2")` as the reader, with the first item described as the current value.
- `entry.operation` checked (`Put` versus `Delete`/`Purge`) before treating the value as a flag.

**Must not produce**:
- `store.watch(key)` presented as delivering the current value.
- A `get` followed by `watch` presented as race-free.
- Redis kept for the flags "because KV has no watch".

### Eval 6 - worker shutdown

**Prompt**: "Our billing worker runs in the same service as axum: `main` connects one `async_nats::Client`, spawns the worker with `tokio::spawn(billing::run(jetstream.clone()))`, and the worker loops over `consumer.messages()` of the durable pull consumer `billing` until the process dies. Add graceful shutdown: on SIGTERM, after HTTP has drained, the worker stops cleanly and nothing the service already published is lost."

**Fixture**: empty

**Must produce**:
- Shutdown through `tokio::sync::watch` (or an equivalent signal) checked in a `tokio::select!`.
- After the stop signal: the worker join, then `flush()`, then `drain()`, in that order.
- Each shutdown wait (the join, `flush()`, `drain()`) bounded by `tokio::time::timeout`.
- A failed or timed-out join that does not skip `flush()` and `drain()`.

**Must not produce**:
- An unbounded `client.flush().await?` at shutdown.
- `drain()` called before the worker is joined.
- More client calls after `drain()`.

### Probe 1 - one await on a JetStream publish

**Prompt**: "Publish an `OrderPlaced` JSON event to JetStream with a dedup id and return the stream sequence."

**Wrong answer**: `(?<!=\s*(?:\w+\s*\.\s*)*)\b(?:js|jetstream|ctx|context)\s*\.\s*(?:send_)?publish\([^;]*?\)\s*\.await\??\s*;|let\s+(?:mut\s+)?(\w+)\s*=\s*(?:\w+\s*\.\s*)*(?:js|jetstream|ctx|context)\s*\.\s*(?:send_)?publish\([^;]*?\)\s*\.await\??\s*;(?![\s\S]*\b\1\s*\.await)|\bPublish::build\(\)`

**Right answer**: `\.await\?\s*(?://[^\n]*)?\s*\.await\?|let\s+(\w+)\s*=[^;]*publish\([^;]*\.await\?\s*;[\s\S]*?\b\1\s*\.await`

### Probe 2 - skipping a stream error

**Prompt**: "My pull worker (`while let Some(m) = messages.next().await { let m = m?; handle(m).await; }`) went silent after the consumer was deleted and recreated. Fix the loop."

**Wrong answer**: `messages\.next\(\)[\s\S]{0,300}?(?:\bErr\(\w*\)\)?\s*=>\s*(?:\{[^}]{0,160}?)?|let\s+Ok\(\w+\)\s*=\s*\w+\s*else\s*\{[^}]{0,160}?)\bcontinue\b(?!\s*')|MissingHeartbeat\s*\)?\s*=>\s*\{\s*(?:tracing::)?(?:warn|debug|info|error)!\([^;]*\);\s*(?:continue;?\s*)?\}`

### Probe 3 - TimedOut as a gateway error

**Prompt**: "In the axum handler, turn the errors from our NATS request to the pricing service into HTTP statuses: no responders, timed out, and anything else."

**Wrong answer**: `TimedOut\b[^\n]{0,60}=>[^\n]{0,60}(?:GATEWAY_TIMEOUT|BAD_GATEWAY)`

### Probe 4 - recreating a consumer to change it

**Prompt**: "Raise `max_deliver` on our existing durable consumer `billing` from 5 to 10 when the service deploys."

**Wrong answer**: `\.delete_consumer\([^)]*\)\s*\.await`

**Right answer**: `create_consumer\(`

### Probe 5 - watching a flag without its current value

**Prompt**: "Read the `checkout.v2` feature flag from a NATS KV bucket at startup and react to every change."

**Wrong answer**: `^(?![\s\S]*\.entry\()[\s\S]*?let\s+(?:mut\s+)?\w+\s*=\s*[\w.]*\.watch\(`
