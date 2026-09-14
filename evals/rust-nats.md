# rust-nats

### Triggering

**Should load**

1. "Add a NATS publisher to the orders service: when POST /orders succeeds, publish an OrderPlaced event to JetStream so the billing worker can pick it up."
2. "In nats-py I did `await js.pull_subscribe('orders.>', durable='billing')` and `msg.ack()`. What is the async-nats equivalent, and how do I run it as a background task in axum?"
3. "My worker stops consuming after a NATS restart. The logs show `consumer deleted` once and then nothing; the process is still up and /health/ready still says `\"messaging\": \"ok\"`."
4. "Request-reply to the pricing service returns `Error { kind: NoResponders, .. }` in staging but works locally. How should the handler map that, 502 or 503?"
5. "Write integration tests for our JetStream consumer with testcontainers — the same reuse-by-name pattern we use for Postgres, and make sure a duplicate Nats-Msg-Id is dropped."

**Should not load**

1. "Cache the GET /users/{id} response in Redis for 60 seconds with invalidation on update." -> `rust-redis`
2. "Add a `/health/ready` endpoint that checks the database pool." -> `axum-service`
3. "Write a sea-orm migration that adds a unique index on `processed_events.event_id`." -> `sea-orm-postgres`
4. "Set up the rstest fixture and the `test_app` helper for the users API tests." -> `rust-testing`
5. "Why does `cargo deny check` fail on a CC0 license after I added a crate?" -> `rust-tooling`

### Eval 1 - durable publish from a handler

**Prompt**: "Add JetStream publishing to POST /orders: an `OrderPlaced` event with the order id, deduplicated so a client retry does not create two events. Map NATS failures onto AppError."

**Must produce**:
- `async-nats = "0.50"` as the only new dependency, `async_nats::Client` and `jetstream::Context` added to `AppState` by value (no `Arc`)
- `jetstream::new(client)` without `.await`
- `send_publish(subject, PublishMessage::build().payload(..).message_id(event_id))` with **two** awaits, `event_id` generated once on the event, not per attempt
- a stream declared at startup with a `duplicate_window`, and the declaration retried with backoff when it returns `TimedOut` (the client may still be connecting under `retry_on_initial_connect`)
- a `MessagingError` (or equivalent) with `#[from]` arms for `RequestError`, `PublishError`, JetStream `PublishError`, inspected through `err.kind()`, and a `fn messaging_error(MessagingError) -> AppError` applied at the call site with `.map_err(messaging_error)?`: `NoResponders`, `TimedOut`, `StreamNotFound` and every other bus failure become `AppError::Unavailable` (503, constant body, the string logged); `MaxPayloadExceeded` and `InvalidSubject` become `BadRequest` (400); the rest `Other` (500)
- a subject of the form `orders.placed.<id>`

**Must not produce**:
- a single await on the JetStream publish
- `jetstream::context::Publish::build()` (deprecated since 0.44)
- a new `AppError::Messaging` variant, a `From<MessagingError> for AppError` impl, or any edit to `error.rs`
- a 502 or 504 for `TimedOut`, or "a responder exists and is slow" as its meaning
- a new client per request, or `Arc<async_nats::Client>`
- `Nats-Msg-Id` described as making the consumer idempotent
- the `nats` crate

### Eval 2 - the billing worker

**Prompt**: "Write the billing worker that consumes `orders.placed.*` from the ORDERS stream. It must survive NATS restarts, not lose messages, stop cleanly on shutdown, and send poison messages somewhere we can inspect."

**Must produce**:
- a durable **pull** consumer via `get_or_create_consumer` (or `create_consumer`, described as create-or-update) with `AckPolicy::Explicit`, `ack_wait`, `max_deliver`, `filter_subject`, `max_ack_pending`
- `consumer.messages()` for the long-lived loop (not `fetch()` in a `loop {}`)
- an outer loop that treats the first `Err` item from `messages()` as terminal for that stream: log, sleep with backoff, rebuild the consumer, continue (no bare `?` that returns from the task, no `continue` that leaves the stream pending)
- handler as a plain function returning an outcome; the loop acks: `double_ack()` on success, `ack_with(AckKind::Nak(Some(delay)))` for transient failure, `ack_with(AckKind::Term)` for undecodable/poison
- `.map_err(|err| anyhow::anyhow!(err))` (or equivalent) on `ack`/`info` results
- shutdown through `tokio::sync::watch` (or an equivalent signal) checked in a `tokio::select!`, then the join, `flush()` and `drain()` in that order, each under `tokio::time::timeout`, a failed join not skipping the rest; the reopen backoff pause also ends on the stop signal (`timeout(backoff, stop.changed())` or a `select!`), not a bare `sleep`
- poison handled as: publish the original payload and headers to `dlq.<original subject>` (a durable two-await publish), *then* `ack_with(AckKind::Term)`; a dead-letter stream capturing `dlq.orders.>` plus `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.ORDERS.>` for messages that exhausted `max_deliver`

**Must not produce**:
- a push consumer or `deliver_subject`
- an in-handler retry loop
- `message.in_progress()` or `message.respond()`
- "`messages()` ends" or "the stream returns `None`" as the reason the outer loop exists
- `delete_consumer` followed by create as the way to change the consumer's config
- an unbounded `client.flush().await?` at shutdown, `drain()` before the worker is joined, or `drain()` followed by more client calls
- `tokio_util::sync::CancellationToken` presented as already in the stack without adding the dependency
- `Term` alone presented as routing a poison message to the DLQ (it fires `MSG_TERMINATED`, which a max-deliveries listener never sees)
- `ack_wait` and `backoff` both set on the consumer without noting that `backoff[0]` replaces `ack_wait`

### Eval 3 - flaky NATS tests under nextest

**Prompt**: "Our NATS integration tests are flaky under nextest: sometimes `409 container name already in use`, sometimes `expected INFO, got nothing`, and two tests occasionally see each other's messages. Fix the harness."

**Must produce**:
- a reused container (`with_container_name` + `ReuseDirective::Always`) with `.with_tag("2.12-alpine")` and `NatsServerCmd::default().with_jetstream()` passed by reference through `with_cmd`
- a retry loop around `start()` explaining the nextest process-per-test race, and a retry loop around `connect()` for the port-proxy race
- `TEST_NATS_URL` short-circuiting the container
- a per-test uuid prefix on every subject, stream and bucket, plus `delete_stream` at the end of each test
- `tokio::time::timeout` around every wait on a subscription or consumer stream
- a note that a `LEAK` status from nextest is a pass

**Must not produce**:
- a `static`/`OnceLock` container presented as shared across tests under nextest
- `FLUSH`-style server-wide cleanup or a fixed stream name shared by tests
- the module's default tag (`2.10.14`) left in place
- a mocked NATS client instead of a server

### Eval 4 - nats-py request-reply port

**Prompt**: "Convert this nats-py service to Rust: `nc.subscribe('pricing.quote', queue='pricing', cb=handler)` where the handler calls `await msg.respond(price)`; the client side does `await nc.request('pricing.quote', b'sku', timeout=0.5)` and catches `NoRespondersError`."

**Must produce**:
- `client.queue_subscribe("pricing.quote", "pricing".to_owned())` driven as a `Stream` with `while let Some(message) = requests.next().await`
- replying by `client.publish(reply, ..)` to `message.reply`, with the `None` case skipped (no `respond` in core NATS)
- `send_request("pricing.quote", Request::new().payload(..).timeout(Some(Duration::from_millis(500))))`
- matching `err.kind() == RequestErrorKind::NoResponders` versus `TimedOut`, both mapped to `AppError::Unavailable` (503) through `messaging_error`, with the note that `TimedOut` is also what a disconnected client returns and therefore is not a 504
- a mention that `ConnectOptions` needs `.retry_on_initial_connect()` because nats-py retries the first connect and async-nats does not

**Must not produce**:
- `message.respond(..)` on a core `async_nats::Message`
- matching on the error enum variants instead of `kind()`
- a callback-style subscribe API
- `no responders` described as arriving immediately regardless of connection state

### Eval 5 - a feature-flag watcher

**Prompt**: "Replace our Redis feature flags with a NATS KV bucket: the service needs the current value of `checkout.v2` at startup and every change afterwards."

**Must produce**:
- `create_key_value` / `get_key_value` for the bucket and `watch_with_history("checkout.v2")` (or `get` followed by `watch`) as the reader, with the first item described as the current value
- `entry.operation` checked (`Put` versus `Delete`/`Purge`) before treating the value as a flag

**Must not produce**:
- `store.watch(key)` presented as delivering the current value
- Redis kept for the flags "because KV has no watch"
