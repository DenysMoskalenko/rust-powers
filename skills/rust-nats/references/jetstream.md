# JetStream: streams, publishing, KV and object store

- [Core NATS or JetStream](#core-nats-or-jetstream)
- [The context](#the-context)
- [Declaring a stream](#declaring-a-stream)
- [Publishing needs two awaits](#publishing-needs-two-awaits)
- [Deduplication with Nats-Msg-Id](#deduplication-with-nats-msg-id)
- [Publish errors](#publish-errors)
- [KV buckets](#kv-buckets)
- [Object store](#object-store)

## Core NATS or JetStream

Core NATS is a router, not a queue. A message is delivered to whoever is subscribed at that
instant and then forgotten: no persistence, no replay, no acknowledgement, no redelivery. That is
the right trade for request-reply, for a cache-invalidation ping and for telemetry fan-out.

JetStream is the persistence layer on top of the same connection. A stream captures subjects to
disk, consumers track their own position, and an unacknowledged message comes back. Anything whose
loss would be a bug — an order placed, a payment captured, an email to send — goes through
JetStream.

A single process uses both: `async_nats::Client` for core, `jetstream::Context` for durable work,
one TCP connection underneath.

## The context

`jetstream::new(client)` wraps the client and spawns one background acker task. It does not talk
to the server, so it is not `async` and cannot fail — a `.await?` on it does not compile. Build it
once; the context is `Clone` and costs nothing to clone into a task.

`jetstream::with_domain(client, "hub")` is the leaf-node form; `ContextBuilder` sets `timeout`
(5 s), `max_ack_inflight` (5000) and `backpressure_on_inflight` (already `true`: a publish waits
instead of failing when the in-flight limit is hit) for the publish side. Every JetStream API call
— stream and consumer info, create, delete, `fetch` — is a `client.request` underneath and is
bounded by the client's `request_timeout`, not by the context's `timeout`.

## Declaring a stream

Streams are declared at startup, from the service that owns the subjects, and never from a
consumer. Three calls, three behaviours:

- `get_or_create_stream(config)` creates the stream or returns the existing one **without
  touching its config**: a retention or subject change in source keeps running with yesterday's
  settings, silently. The right call when the service does not own the stream's shape.
- `create_stream(config)` is idempotent for the same config and fails with `code 400, err 10058:
  stream name already in use with a different configuration` on a changed one — the drift check.
  The owning service uses it when a silent mismatch is worse than a failed deploy.
- `create_or_update_stream(config)` pushes the new config, widening or narrowing subjects under a
  running consumer; it returns `Info`, so follow it with `get_stream(name)` for the handle.

Startup must survive the first call timing out: with `retry_on_initial_connect()` the client may
still be connecting, and the request fails with `TimedOut` after `request_timeout`. Retry with
backoff (the startup code in the service-integration reference does).

Two subjects lists that overlap across two streams make a publish ambiguous and the server rejects
the second stream, so subject ownership is exclusive per stream.

```rust,verify
//! Declaring a stream and publishing to it.
use std::time::Duration;

use async_nats::jetstream::{
    self, Context,
    context::PublishErrorKind,
    message::PublishMessage,
    stream::{self, DiscardPolicy, RetentionPolicy, StorageType},
};

/// Not async, cannot fail: it only wraps the client.
pub fn context(client: async_nats::Client) -> Context {
    jetstream::new(client)
}

/// Called once at startup by the service that owns `orders.>`.
pub async fn ensure_stream(ctx: &Context) -> anyhow::Result<stream::Stream> {
    Ok(ctx
        .get_or_create_stream(stream::Config {
            name: "ORDERS".to_owned(),
            subjects: vec!["orders.>".to_owned()],
            // Limits: keep until max_age or max_bytes, every consumer sees
            // everything. WorkQueue: delete on ack, one consumer per subject.
            // Interest: keep only while some consumer still wants it.
            retention: RetentionPolicy::Limits,
            storage: StorageType::File,
            max_age: Duration::from_hours(24 * 7),
            max_messages: 1_000_000,
            // The default, `Old`, silently drops the oldest message when a
            // limit is hit. `New` refuses the publish instead, which is the
            // only honest answer for a stream of orders.
            discard: DiscardPolicy::New,
            // Any republish of the same `Nats-Msg-Id` inside this window is
            // dropped by the server. Two minutes is the NATS default.
            duplicate_window: Duration::from_secs(120),
            // 3 in a cluster, 1 on a single server; a mismatch fails the create.
            num_replicas: 1,
            ..Default::default()
        })
        .await?)
}

/// The double await is the API, not a typo. The first await sends the publish
/// and resolves to a `PublishAckFuture`; the second waits for the stream's ack.
/// One await compiles, reports success, and loses every message the stream
/// rejected.
pub async fn publish(ctx: &Context, order_id: &str, event_id: &str) -> anyhow::Result<u64> {
    let ack = ctx
        .send_publish(
            format!("orders.placed.{order_id}"),
            PublishMessage::build()
                .payload(br#"{"ok":true}"#.to_vec().into())
                // Sets the `Nats-Msg-Id` header: server-side deduplication.
                .message_id(event_id),
        )
        .await?
        .await?;
    tracing::info!(
        stream = %ack.stream,
        sequence = ack.sequence,
        duplicate = ack.duplicate,
        "published"
    );
    Ok(ack.sequence)
}

/// `StreamNotFound` is what "no responders" looks like on a `JetStream` publish:
/// nothing is capturing that subject, usually because startup did not run
/// `ensure_stream` or the subject has a typo.
pub fn classify(err: &jetstream::context::PublishError) -> &'static str {
    match err.kind() {
        PublishErrorKind::StreamNotFound => "no stream captures this subject",
        PublishErrorKind::TimedOut => "the stream did not ack in time",
        PublishErrorKind::WrongLastSequence | PublishErrorKind::WrongLastMessageId => {
            "an optimistic-concurrency expectation failed"
        }
        _ => "publish failed",
    }
}

```

## Publishing needs two awaits

`ctx.publish(subject, payload)` and `ctx.send_publish(subject, PublishMessage)` both return a
future that resolves to a `PublishAckFuture`. Awaiting the first only means the bytes left the
process; awaiting the second means the stream wrote them and answered with `PublishAck { stream,
sequence, domain, duplicate }`. Code that stops at one await compiles clean and drops messages
whenever the stream is full, missing or out of replicas.

`PublishMessage::build()` is the builder; `jetstream::context::Publish` — what older examples
use — has been deprecated since 0.44 and fails `-D warnings`.

The client-side limit on unacked publishes is `ContextBuilder::max_ack_inflight` (5000);
`backpressure_on_inflight` is on by default, so the publish future waits instead of failing when
the limit is reached.

## Deduplication with Nats-Msg-Id

`.message_id(id)` sets the `Nats-Msg-Id` header. The server remembers ids for the stream's
`duplicate_window` and answers a repeat with the **original** sequence and `duplicate: true`, so a
retried publish after a lost ack is harmless. That is publish-side idempotency only: a consumer
that crashed after processing but before acking still sees the message again. The consumer half
is a unique key in Postgres on the same id, written in the handler's transaction.

Use the event's own id, generated once when the event is created, never a fresh uuid per publish
attempt.

## Publish errors

`jetstream::context::PublishError` is `Error<PublishErrorKind>`: a struct with `kind()`, not an
enum. `StreamNotFound` means nothing captures the subject — the `no responders` of JetStream —
and it arrives after the **second** await. `TimedOut` is the context's `timeout` (5 s default)
elapsing without an ack: a slow stream, or a NATS outage — a disconnected client does not fail
fast, it waits the whole timeout. `WrongLastSequence` and `WrongLastMessageId` are the optimistic
concurrency checks `PublishMessage::expected_last_sequence` / `expected_last_message_id`
failing. A payload larger than the server's `max_payload` (1 MiB default) fails client-side with
`MaxPayloadExceeded` before anything is sent, on core `publish` and `request` too.

Core `publish` behaves differently during an outage: it returns `Ok` at once, because the message
went into the connection's 2048-slot command buffer, and only blocks once that fills; `flush()`
is the call that actually waits for the server and therefore the only one that notices.

## KV buckets

A KV bucket is a stream named `KV_<bucket>` with a key-value facade: `put` returns the revision,
`get` returns `Option<Bytes>`, `entry` returns the value with its revision and operation,
`update(key, value, expected_revision)` is compare-and-swap, `create` is put-if-absent, and
`watch(key)` yields every change made **after the call** as a stream of `Entry` — not the
current value; `watch_with_history(key)` (and `watch_many_with_history`) delivers the current
value of each matching key first, then every change. `history: 1` makes it a last-value cache;
higher keeps revisions. This is what replaces "Redis for feature flags": a flag reader uses
`watch_with_history`; a `get` followed by `watch` loses any change made between the two calls. For an actual cache with TTLs see
`rust-redis`.

```rust,verify
//! KV bucket and object store.
use std::time::Duration;

use async_nats::jetstream::{Context, kv, object_store, stream::StorageType};
use tokio_stream::StreamExt as _;

pub async fn write_flags(ctx: &Context) -> anyhow::Result<kv::Store> {
    let store = ctx
        .create_key_value(kv::Config {
            bucket: "feature-flags".to_owned(),
            history: 5,
            max_age: Duration::from_secs(0), // 0 = keep forever
            storage: StorageType::File,
            num_replicas: 1,
            ..Default::default()
        })
        .await?;

    let revision = store.put("checkout.v2", "on".into()).await?;
    let current = store.get("checkout.v2").await?; // Option<Bytes>
    anyhow::ensure!(current.is_some(), "the flag was just written");

    // Compare-and-swap: fails if anyone wrote in between.
    store.update("checkout.v2", "off".into(), revision).await?;
    // Put-if-absent.
    let _first = store.create("rollout.started", "1".into()).await;
    Ok(store)
}

/// `watch_with_history` delivers the current value of every matching key
/// first, then each change. Plain `watch` starts at "now" and a reader that
/// uses it never learns a flag's value until someone changes it.
pub async fn watch_flags(store: &kv::Store) -> anyhow::Result<()> {
    let mut changes = store.watch_with_history("checkout.>").await?;
    while let Some(entry) = changes.next().await {
        let entry = entry?;
        tracing::info!(key = %entry.key, revision = entry.revision, op = ?entry.operation, "kv change");
    }
    Ok(())
}

/// Chunked blobs over a stream named `OBJ_<bucket>`. Reasonable to tens of
/// megabytes per object; beyond that use S3 and publish the key.
pub async fn invoices(ctx: &Context) -> anyhow::Result<()> {
    let store = ctx
        .create_object_store(object_store::Config {
            bucket: "invoices".to_owned(),
            max_bytes: 256 * 1024 * 1024,
            storage: StorageType::File,
            ..Default::default()
        })
        .await?;
    let mut data = std::io::Cursor::new(b"%PDF-1.7".to_vec());
    let info = store.put("invoice-1.pdf", &mut data).await?;
    let _object = store.get("invoice-1.pdf").await?; // implements AsyncRead
    tracing::info!(size = info.size, chunks = info.chunks, "stored");
    Ok(())
}

```

`get_key_value(name)` fetches an existing bucket, `create_or_update_key_value` reconciles config,
`delete_key_value` removes it with its history. `keys()` lists, `history(key)` replays revisions,
`purge(key)` deletes the history too where `delete` only writes a tombstone. `watch_all()` is
`watch(">")`; every watcher is a `Stream` that ends only when dropped.

## Object store

`create_object_store` / `get_object_store` / `delete_object_store` on the context;
`put(name, &mut impl AsyncRead)`, `get(name)` returning an `AsyncRead`, `info`, `list`, `watch`,
`delete`, `seal` and `add_link` on the store. Objects are chunked across `OBJ_<bucket>`, so
every byte is replicated `num_replicas` times and a 100 MB upload is a 300 MB write on a
three-node cluster. Keep objects to tens of megabytes; past that, S3 holds the bytes and a NATS
message carries the key.
