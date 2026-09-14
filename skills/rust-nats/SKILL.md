---
name: rust-nats
description: "Use when publishing or consuming NATS messages from an axum service — an event bus, work queue or message queue on async-nats: connect options, subjects, request-reply, queue groups, JetStream streams and durable pull consumers, Nats-Msg-Id deduplication, ack, nak and term, KV buckets, tests against a NATS container. Also for no responders, consumer deleted, timed out, MAX_DELIVERIES. Not for Redis caching or pub/sub (rust-redis), nor the readiness endpoint itself (axum-service)."
metadata:
  version: "0.1.0"
---

# NATS with async-nats

Assumes Rust 1.98 edition 2024, tokio 1, axum 0.8, async-nats 0.50 with default features,
NATS server 2.12 with JetStream, testcontainers-modules 0.15.

## Important

- Every worker is a **durable pull consumer with explicit ack**; never a push consumer.
- A JetStream publish is two awaits: `.await?.await?`. One await sends and loses the rejection.
- The first `Err` item from `consumer.messages()` is terminal for that stream: log, back off,
  rebuild the consumer, loop. `?` kills the worker; `continue` hangs it.
- Ack after the database commit. `Nats-Msg-Id` deduplicates the publish only; consumer
  idempotency is a unique key in Postgres.
- `AppError` gains no variant and no `From<MessagingError>`: map with `messaging_error` onto
  `Unavailable` (503), `BadRequest` (400) or `Other` (500).

NATS is the message bus, not a cache (`rust-redis`). The legacy `nats` crate is deprecated;
`async-nats` is the only client.

```toml
async-nats = "0.50"   # jetstream, kv, object-store and service are default features
```

Pre-1.0, a breaking minor every few weeks: pin it. Rustls only. Subscriptions and
consumer streams are `Stream`s, driven with `tokio_stream::StreamExt`.

## Decide first

| Need | Use | Because |
|---|---|---|
| Fire-and-forget notification, RPC, fan-out | core `Client` | routed to whoever is subscribed now, then forgotten |
| Anything whose loss is a bug | JetStream stream + durable pull consumer | persisted, acked, redelivered |
| Flags, small config, last value per key | KV bucket with `watch_with_history` | the current value first, then every change |
| Blobs up to tens of MB | object store | past that, S3 and a message with the key |

## The core pattern

Publish durably, consume durably; the rest refines this block.

```rust,verify
//! A durable publish and the pull consumer that reads it.
use std::time::Duration;

use async_nats::jetstream::{
    AckKind, Context,
    consumer::{AckPolicy, pull},
    message::PublishMessage,
    stream,
};
use tokio_stream::StreamExt as _;

/// `message_id` is the event's own id, minted once, so a retried publish is deduplicated.
pub async fn publish_order_placed(ctx: &Context, order_id: &str, event_id: &str) -> anyhow::Result<u64> {
    let message = PublishMessage::build().payload(br#"{"ok":true}"#.to_vec().into()).message_id(event_id);
    let ack = ctx
        .send_publish(format!("orders.placed.{order_id}"), message)
        .await? // sent
        .await?; // the stream wrote it: PublishAck { sequence, duplicate, .. }
    Ok(ack.sequence)
}

/// Runs until the first `Err` item, then returns so the caller can back off,
/// rebuild the consumer and call again. Never `?` inside the loop, never `continue`.
pub async fn consume_until_error(stream: &stream::Stream) -> anyhow::Result<()> {
    let consumer = stream
        .get_or_create_consumer(
            "billing",
            pull::Config {
                durable_name: Some("billing".to_owned()),
                ack_policy: AckPolicy::Explicit,
                ack_wait: Duration::from_secs(30), // longer than the slowest handler
                max_deliver: 5,                    // bounds a poison message
                filter_subject: "orders.placed.*".to_owned(),
                max_ack_pending: 256, // backpressure: the server stops past this
                ..Default::default()
            },
        )
        .await?;
    let mut messages = consumer.messages().await?;
    loop {
        let message = match messages.next().await {
            Some(Ok(message)) => message,
            Some(Err(err)) => return Err(err.into()),
            None => anyhow::bail!("message stream ended"),
        };
        // Handle, commit, ack; `Nak(Some(delay))` retries. Poison: copy to
        // `dlq.<subject>`, then `Term` — alone, `Term` reaches no DLQ.
        if serde_json::from_slice::<serde_json::Value>(&message.payload).is_ok() {
            message.double_ack().await.map_err(|err| anyhow::anyhow!(err))?;
        } else {
            let copy = PublishMessage::build()
                .payload(message.payload.clone())
                .headers(message.headers.clone().unwrap_or_default());
            message.context.send_publish(format!("dlq.{}", message.subject), copy).await?.await?;
            message.ack_with(AckKind::Term).await.map_err(|err| anyhow::anyhow!(err))?;
        }
    }
}
```

## Connect once, clone everywhere

`async_nats::Client` is a cheap `Clone` handle over one multiplexed connection; `jetstream::new`
wraps it synchronously, cannot fail, and spawns one acker task. Both live in `AppState` by value
(`nats: async_nats::Client`, `jetstream: jetstream::Context`) — never `Arc` them. One connection
per process.

Four `ConnectOptions` are not defaults and must be set: `.name("service")`,
`.request_timeout(Some(2 s))` (the default 10 s is the whole outbound-HTTP budget and bounds every
JetStream API call), `.retry_on_initial_connect()` (without it a NATS that boots second crash-loops
the pod; with it the first JetStream call may `TimedOut`, so the startup stream declaration
retries) and `.event_callback(..)` to log `Connected`, `Disconnected`, `SlowConsumer`. Auth:
`.token(..)`, `with_credentials_file(..)`, `with_nkey(..)`.

## Subjects and publish

Subjects are dot-separated, lowercase, general to specific, the id last: `orders.placed.<order_id>`.
`*` matches one token, `>` matches the rest. A consumer filters on `orders.placed.*`; a metric
labels by the family `orders.placed` — an id in a label is unbounded cardinality.

Core `publish` returns once the bytes are queued locally; during an outage it still returns `Ok`
(buffered, blocking once 2048 pile up). Only `flush()` notices — at shutdown and in tests.

JetStream `send_publish` is the two-await call above. `message_id` sets `Nats-Msg-Id`; a repeat
inside the stream's `duplicate_window` (2 min default) returns the original sequence with
`duplicate: true`. During an outage the second await fails with `TimedOut` after the context
timeout (5 s). `jetstream::context::Publish` is deprecated since 0.44 and fails `-D warnings`.

Streams are declared at startup by the service that owns the subjects: `get_or_create_stream`
returns an existing stream untouched, `create_stream` fails on a changed config (err 10058),
`create_or_update_stream` reconciles.

## Consume

`get_or_create_consumer` never reconciles; `create_consumer(config)` creates or updates in place,
cursor kept (`deliver_policy`, `ack_policy`, `replay_policy` and the durable name are immutable:
err 10012). Never delete and recreate — that drops the cursor and every pending message.

`consumer.messages()` is an endless stream for a long-lived worker, pre-pulling up to 200
messages; `fetch().max_messages(n).expires(d).messages()` is one bounded batch for cron-shaped
work and tests. `messages()` yields a typed `MessagesError`; `fetch()`, `ack()`, `double_ack()`,
`ack_with()` and `info()` return `async_nats::Error`, a boxed `dyn Error` that needs
`.map_err(|err| anyhow::anyhow!(err))?`.

The handler is a plain function from a typed event to an `Outcome` — `Done`, `Retry(delay)`,
`Poison(reason)` — and the loop acks:

| Outcome | Call | Note |
|---|---|---|
| done | `double_ack()` | waits for the server; `ack()` when a redelivery is harmless |
| transient failure | `ack_with(AckKind::Nak(Some(delay)))` | the server retries; no retry loop in the handler |
| poison, undecodable | copy to `dlq.<subject>`, then `ack_with(AckKind::Term)` | stops redelivery now |
| still working | `ack_with(AckKind::Progress)` | extends `ack_wait`; there is no `in_progress()` |

The copy carries payload and headers; `Term` alone fires `MSG_TERMINATED`, not
`MAX_DELIVERIES`. Past `max_deliver` the server fires
`$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<stream>.<consumer>` on the next delivery attempt; one
stream capturing `dlq.orders.>` and that advisory is the DLQ.

Trouble arrives as `Err` items — `ConsumerDeleted`, `NoResponders` (consumer gone),
`MissingHeartbeat`, `Pull` — after which the stream stays pending; the outer loop rebuilds. Only
the shutdown signal (a `tokio::sync::watch`, one task per consumer) ends it. After HTTP has
drained: flip the watch, join, `flush()`, `drain()`, each under `tokio::time::timeout`.

## Request-reply

`client.request(subject, payload)` uses the client-wide timeout; `send_request(subject,
Request::new().payload(..).timeout(Some(..)))` sets one per call. `no responders` comes back
immediately from a connected server; a disconnected client waits out the timeout and gets
`TimedOut`. The responder is a `queue_subscribe` loop that publishes to `message.reply` — there
is no `Message::respond`, and `reply` is `None` when the sender used `publish`.
`client.service_builder()` (`service::ServiceExt as _`) adds `nats.micro` endpoints.

## Errors

`PublishError`, `RequestError`, `SubscribeError` and JetStream's `PublishError` are
`Error<Kind>` structs: match on `err.kind()`, not on the error. One `MessagingError` enum wraps
the four with `#[from]`, and `fn messaging_error(MessagingError) -> AppError` maps it at the call
site (`.map_err(messaging_error)?`) onto the canonical variants — `AppError` itself is untouched:

| Kind | `AppError` | Meaning |
|---|---|---|
| `NoResponders`, `TimedOut`, `Send`, `StreamNotFound`, `BrokenPipe`, `MaxAckPending` | `Unavailable(String)` 503 | nothing subscribed, nothing captured the subject, or NATS is down — the string is logged, the body is constant |
| `MaxPayloadExceeded`, `InvalidSubject` on publish or request | `BadRequest(String)` 400 | refused client-side before sending |
| `WrongLastSequence`, subscribe-side `InvalidSubject`, `Other` | `Other(anyhow::Error)` 500 | a wiring or programming error |

`TimedOut` is not 504: a disconnected client returns it too. No 502 — that is `axum-service`'s
`Http` variant for outbound HTTP.

## Readiness

`/health/ready` is `axum-service`'s: it returns `{ "status", "checks": { .. } }` and the status
rule. This skill may add one entry, `"messaging"`: `Health::Ok` when `client.connection_state()`
is `Connected` (local, no round trip), else `Health::Degraded` — optional, so the probe stays 200.
Adding it changes the body `tests/health.rs` asserts.

## Envelopes, idempotency, telemetry

One serde JSON struct per subject family with its own `event_id: Uuid`: the `Nats-Msg-Id` on
publish, and on the consumer side a unique column written in the same transaction as the work, a
unique violation being `Done` (`sea-orm-postgres`). A decode failure is `Poison`, never `Retry`.

The producer span injects `traceparent` into the `HeaderMap` with the global propagator; the
consumer span extracts it and `set_parent`s, so request and worker are one trace. Fields follow
the OTel `messaging.*` conventions; the span name is `{operation} {family}` — `process
orders.placed`, never the full subject. Counters `nats_messages_published_total` and
`nats_messages_consumed_total` by family. Propagator and exporter belong to `axum-service`.

## Testing

Tests run against a real server: `TEST_NATS_URL` from compose or CI (`rust-tooling`'s
optional-services block), or a testcontainers NATS
started with `NatsServerCmd::default().with_jetstream()` and `.with_tag("2.12-alpine")` — the
module default is `2.10.14`. Reuse by name races under nextest (a Docker 409, then `expected INFO,
got nothing` for an early attacher); both retries are in the harness. Isolate with a per-test uuid
prefix on every subject, stream and bucket; wrap every wait in `tokio::time::timeout`. Handlers
are tested without a broker.

## Red Flags — STOP

| About to… | Rule |
|---|---|
| Put `Arc<async_nats::Client>` in state, or connect per request | A cheap `Clone` handle; one connection per process |
| Await a JetStream publish once | The first await only sends; `.await?.await?` or the rejection is lost |
| Treat core `publish` as delivered | It queued bytes locally; JetStream for anything that must arrive |
| `?` on an item from `consumer.messages()` | The stream never ends; the first `Err` means rebuild and loop — `?` kills the worker, `continue` hangs it |
| `ack()` before the transaction commits | A crash in between loses the message; ack after |
| Leave `max_deliver` unset | A poison message is redelivered forever; set it, capture the advisory |
| `Term` poison and call that the DLQ | `Term` fires `MSG_TERMINATED` only; copy to `dlq.<subject>` first |
| Rely on `Nats-Msg-Id` for consumer idempotency | It dedups the publish only; unique key in Postgres |
| Create a push consumer | Flow control is opt-in and per subscription there; durable pull has it built in |
| Reconcile a consumer with `delete_consumer` + create | Drops the cursor and every pending message; `create_consumer` updates in place |
| Add a `Messaging` variant or `From<MessagingError>` to `AppError` | `AppError` is `axum-service`'s; map at the call site with `messaging_error` |
| Map `TimedOut` to 502 or 504 | 503 `Unavailable`; a disconnected client times out too |
| `flush()` or `drain()` at shutdown without a timeout | `flush()` blocks while NATS is down; `tokio::time::timeout` around every shutdown wait |
| `watch(key)` to read a flag | Yields only changes after the call; `watch_with_history`, or `get` then `watch` |
| Use Redis pub/sub for events | No persistence, acks or replay; NATS — caching stays in `rust-redis` |

## References

- `references/jetstream.md` — read when declaring a stream or choosing core versus JetStream:
  the three declaration calls, retention, `duplicate_window`, publish errors, KV and
  `watch_with_history`, object store.
- `references/consumers.md` — read when writing or debugging a worker: the pull config field by
  field, `messages()` versus `fetch()`, ack variants, `dead_letter`, redelivery and the
  dead-letter stream, backpressure, what each `Err` item means, ordered and push consumers.
- `references/service-integration.md` — read when wiring NATS into the service: state and
  settings, publishing from handlers, `MessagingError` and `messaging_error`, the worker task,
  startup retry and bounded shutdown, the readiness check, `traceparent` and `messaging.*`
  spans, the easy-to-get-wrong table.
- `references/testing-nats.md` — read when setting up NATS tests: the container and its tag,
  the two races, prefix isolation, the harness, what is worth a test.
