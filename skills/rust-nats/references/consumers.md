# Consumers: pull, ack, redelivery, dead letters

- [Pull, durable, explicit ack](#pull-durable-explicit-ack)
- [The config, field by field](#the-config-field-by-field)
- [messages() or fetch()](#messages-or-fetch)
- [Two error types](#two-error-types)
- [Ack, nak, term, progress](#ack-nak-term-progress)
- [Redelivery and the dead-letter stream](#redelivery-and-the-dead-letter-stream)
- [Backpressure](#backpressure)
- [An Err item, and what to do with it](#an-err-item-and-what-to-do-with-it)
- [Ordered consumers](#ordered-consumers)
- [Push consumers](#push-consumers)

## Pull, durable, explicit ack

A consumer is a server-side cursor over a stream. *Durable* means the server keeps the cursor
under a name, so a restarted pod resumes where it stopped instead of replaying or skipping. *Pull*
means the client asks for messages and the server hands over at most that many, which is flow
control for free. *Explicit ack* means a message stays pending until the handler says otherwise.
Every worker in this stack is all three.

`get_or_create_consumer(name, config)` is the startup call, same caveat as
`get_or_create_stream`: it returns an existing consumer with whatever config it has.
`create_consumer(config)` is create-or-**update**: an existing durable takes the new `ack_wait`,
`max_deliver`, `backoff`, `max_ack_pending` in place and keeps its cursor; only `deliver_policy`,
`ack_policy`, `replay_policy` and the durable name are immutable and answer `err 10012` (`deliver
policy can not be updated`). `create_consumer_strict` is the one that fails with `AlreadyExists`
when the consumer exists. Never reconcile with `delete_consumer` plus create: it drops the cursor
and every pending message.

## The config, field by field

```rust,verify
//! A durable pull consumer and the loop that drives it.
use std::time::Duration;

use async_nats::jetstream::{
    AckKind, Message,
    consumer::{AckPolicy, DeliverPolicy, PullConsumer, ReplayPolicy, pull},
    stream,
};
use tokio_stream::StreamExt as _;

pub async fn ensure_consumer(
    stream: &stream::Stream,
    durable: &str,
) -> anyhow::Result<PullConsumer> {
    Ok(stream
        .get_or_create_consumer(
            durable,
            pull::Config {
                durable_name: Some(durable.to_owned()),
                ack_policy: AckPolicy::Explicit,
                // Longer than the slowest handler. Past this the server
                // redelivers while the first attempt may still be running.
                // `backoff: vec![..]` would replace it: `backoff[0]` becomes
                // the lease, so set one or the other, never both.
                ack_wait: Duration::from_secs(30),
                // After this many attempts the server gives up on the message
                // and publishes a MAX_DELIVERIES advisory; see the dead-letter
                // section. Unset means forever.
                max_deliver: 5,
                // A subset of the stream. One consumer per business concern,
                // not one per stream.
                filter_subject: "orders.placed.*".to_owned(),
                // All: replay the stream's history on first creation. New:
                // start at the head. Only matters the first time.
                deliver_policy: DeliverPolicy::All,
                // The server stops sending once this many are unacked: a slow
                // handler slows the stream instead of filling memory.
                max_ack_pending: 256,
                replay_policy: ReplayPolicy::Instant,
                ..Default::default()
            },
        )
        .await?)
}

/// `messages()` is an endless stream that keeps its own pull requests in
/// flight. The item is `Result<Message, MessagesError>`; the first `Err` is
/// terminal for this stream and the caller reopens (see "An Err item").
pub async fn consume(consumer: PullConsumer) -> anyhow::Result<()> {
    let mut messages = consumer.messages().await?;
    while let Some(message) = messages.next().await {
        let message = message?;
        // `info()` parses the reply subject, so it is fallible and its error is
        // a boxed `dyn Error` that needs an explicit conversion.
        let info = message.info().map_err(|err| anyhow::anyhow!(err))?;
        tracing::info!(
            stream_sequence = info.stream_sequence,
            delivered = info.delivered,
            pending = info.pending,
            "consuming"
        );
        decide(&message).await?;
    }
    Ok(())
}

/// Every ack variant. All of them return a boxed `dyn Error`, hence the `map_err`.
pub async fn decide(message: &Message) -> anyhow::Result<()> {
    if message.payload.is_empty() {
        // Poison: stop redelivery now instead of burning `max_deliver` attempts.
        // A real worker copies it to `dlq.<subject>` first — `dead_letter` in
        // the dead-letter section; this shows the ack variant alone.
        return message
            .ack_with(AckKind::Term)
            .await
            .map_err(|err| anyhow::anyhow!(err));
    }
    if message.payload.starts_with(b"retry") {
        // Transient failure: the server redelivers after the delay. No
        // in-handler retry loop, it would only burn `ack_wait`.
        return message
            .ack_with(AckKind::Nak(Some(Duration::from_secs(30))))
            .await
            .map_err(|err| anyhow::anyhow!(err));
    }
    if message.payload.starts_with(b"slow") {
        // Extend the lease from inside a long handler. There is no
        // `in_progress()`; this is the `AckKind::Progress` form.
        message
            .ack_with(AckKind::Progress)
            .await
            .map_err(|err| anyhow::anyhow!(err))?;
    }
    // `ack()` is fire-and-forget. `double_ack()` waits for the server to
    // confirm, so a lost ack cannot cause a redelivery the handler cannot
    // absorb. Use it after any side effect that is not idempotent.
    message
        .double_ack()
        .await
        .map_err(|err| anyhow::anyhow!(err))
}

```

`ack_wait` is the lease. `backoff: vec![d0, d1, ..]` replaces it: the server sets `ack_wait` to
`backoff[0]`, the first redelivery waits `backoff[1]`, and the last value repeats — so `ack_wait:
30 s` next to `backoff: [1 s, 10 s]` is a 1 s lease, not 30. Set one or the other, and keep
`max_deliver >= backoff.len()`, which the server requires. `max_ack_pending` is per consumer, not
per process — two pods sharing a durable name split that budget. `filter_subject` is a single pattern;
`filter_subjects` (plural) takes several on server 2.10+.

`deliver_policy` is evaluated only when the consumer is created and cannot be updated
(`err 10012`); it changes only by deleting and recreating, which also resets the cursor.
`inactive_threshold` deletes a consumer after idle time; it defaults to never for a durable one
and applies to durables when set (server 2.9+).

## messages() or fetch()

`consumer.messages().await?` returns a `Stream` that never ends on its own: it issues pull
requests as the buffer drains, 200 messages per pull by default (`consumer.stream()
.max_messages_per_batch(n)` and `.max_bytes_per_batch(n)` tune it, `.heartbeat(d)` the idle
heartbeat, 15 s by default). This is the shape for a long-lived worker. Those 200 are held in the
process: at shutdown whatever was pulled but not handled is redelivered only after `ack_wait`, so
lower the batch where a deploy must not delay messages by that much.

`consumer.fetch().max_messages(n).expires(d).messages().await?` is one bounded batch: it ends
after `n` messages or `d` elapsed, whichever first. This is the shape for cron-style draining,
for tests, and for "process at most 100 then stop". `batch()` is the same without waiting for
`expires` when the batch is already full. Neither belongs in a `loop {}` as a stand-in for
`messages()`: each fetch is a round trip and a pause.

```rust,verify
//! A bounded batch: cron-shaped work and tests.
use std::time::Duration;

use async_nats::jetstream::consumer::PullConsumer;
use tokio_stream::StreamExt as _;

pub async fn drain_once(consumer: &PullConsumer) -> anyhow::Result<usize> {
    let mut batch = consumer
        .fetch()
        .max_messages(100)
        .expires(Duration::from_secs(2))
        .messages()
        .await?;
    let mut count = 0;
    while let Some(message) = batch.next().await {
        // `fetch()` and `batch()` yield `Result<Message, async_nats::Error>`, a
        // `Box<dyn Error + Send + Sync>` that `?` cannot turn into anyhow.
        let message = message.map_err(|err| anyhow::anyhow!(err))?;
        message
            .double_ack()
            .await
            .map_err(|err| anyhow::anyhow!(err))?;
        count += 1;
    }
    Ok(count)
}

```

## Two error types

`messages()` yields `Result<Message, MessagesError>` — a typed `Error<MessagesErrorKind>` that
`?` converts into `anyhow::Error`. `fetch()` and `batch()` yield
`Result<Message, async_nats::Error>`, and so do `ack()`, `double_ack()`, `ack_with()` and
`info()`. `async_nats::Error` is `Box<dyn std::error::Error + Send + Sync>`, and the box does not
implement `Error`, so `?` fails with *`?` couldn't convert the error: `dyn std::error::Error +
Send + Sync: Sized` is not satisfied*. Convert with `.map_err(|err| anyhow::anyhow!(err))?`.

## Ack, nak, term, progress

| Call | Effect | When |
|---|---|---|
| `ack()` | done; sent, not confirmed | side effects are idempotent anyway |
| `double_ack()` | done; waits for the server's confirmation | a redelivery would cost something |
| `ack_with(AckKind::Nak(Some(d)))` | redeliver after `d`; `None` means now | transient failure downstream |
| `ack_with(AckKind::Term)` | never redeliver; counts as handled | poison: undecodable, invalid forever |
| `ack_with(AckKind::Progress)` | reset `ack_wait` | a handler that legitimately outruns the lease |

Ack after the work is committed, never before: a crash between an early ack and the database
commit loses the message. Reverse the order and the same crash costs one redelivery, which the
unique key absorbs.

A `Term` on decode failure is not optional. Without it a message nobody can parse is redelivered
`max_deliver` times, or forever, and the `MAX_DELIVERIES` advisory says "unknown reason". But a
`Term` on its own fires `$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.<stream>.<consumer>`, which a
max-deliveries listener never sees: the message reaches the log and nothing else. Copy it to the
dead-letter subject first, then `Term` — `dead_letter` below.

## Redelivery and the dead-letter stream

Two paths lead to the dead-letter stream, and it captures both:

1. **Poison**, decided by the handler: the worker publishes the original payload and headers to
   `dlq.<original subject>` (`dlq.orders.placed.42`), with a `Dlq-Reason` header, then acks
   `Term`. The copy is a normal message: inspect, fix, republish to the original subject.
2. **Exhausted retries**: a message whose `ack_wait` expires, or that was nak'd, is redelivered
   with `info().delivered` incremented. When `delivered` reaches `max_deliver` the server stops
   and publishes to `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<stream>.<consumer>` a JSON body
   with `stream`, `consumer`, `stream_seq` and `deliveries` — enough to fetch the original with
   `stream.get_raw_message(stream_seq)`, which fails once the source stream's `max_age` or
   `max_messages` has evicted it, so the DLQ handler tolerates a missing original.

The advisory fires on the **next delivery attempt**, not on the final nak. With a `messages()`
pull already waiting it lands within milliseconds of the last nak; with no pull waiting, not
until the next one — a test that naks `max_deliver` times and then waits sees nothing until it
issues one more (empty) fetch.

```rust,verify
//! The dead-letter stream: poison copies plus the max-deliveries advisory.
use async_nats::jetstream::{AckKind, Context, Message, message::PublishMessage, stream};

/// `root` is the source stream's subject root (`orders`), `source` its name (`ORDERS`).
pub async fn ensure_dlq(ctx: &Context, root: &str, source: &str) -> anyhow::Result<stream::Stream> {
    Ok(ctx
        .get_or_create_stream(stream::Config {
            name: format!("{source}_DLQ"),
            subjects: vec![
                format!("dlq.{root}.>"),
                format!("$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.{source}.>"),
            ],
            storage: stream::StorageType::File,
            ..Default::default()
        })
        .await?)
}

/// Poison: copy it where it can be inspected, then stop redelivery. The
/// publish is durable (two awaits) and comes first: a crash in between costs
/// one duplicate copy, the reverse order loses the message.
pub async fn dead_letter(message: &Message, reason: &str) -> Result<(), async_nats::Error> {
    let mut headers = message.headers.clone().unwrap_or_default();
    headers.insert("Dlq-Reason", reason);
    let copy = PublishMessage::build().payload(message.payload.clone()).headers(headers);
    message
        .context
        .send_publish(format!("dlq.{}", message.subject), copy)
        .await?
        .await?;
    message.ack_with(AckKind::Term).await
}

/// What the advisory carries. The `type` is `...advisory.v1.max_deliver`,
/// singular, although the subject token is `MAX_DELIVERIES`.
#[derive(Debug, serde::Deserialize)]
pub struct MaxDeliveries {
    pub stream: String,
    pub consumer: String,
    pub stream_seq: u64,
    pub deliveries: u64,
}

/// Recover the original: the advisory names the sequence, the stream has the bytes.
pub async fn original(
    stream: &stream::Stream,
    advisory: &MaxDeliveries,
) -> anyhow::Result<Vec<u8>> {
    let raw = stream
        .get_raw_message(advisory.stream_seq)
        .await
        .map_err(|err| anyhow::anyhow!(err))?;
    Ok(raw.payload.to_vec())
}

```

A consumer over the DLQ stream is an ordinary durable pull consumer whose handler pages someone,
writes a row, or republishes after a fix; it tells the two shapes apart by subject.

## Backpressure

`max_ack_pending` is the only knob that matters: the server stops delivering once that many
messages are unacked for the consumer, so a slow handler slows the stream rather than the process
running out of memory. Size it to `concurrency × ack_wait ÷ handler latency`, and remember it is
shared across every process bound to the durable name. Below it, `messages()` itself holds up to
200 pre-pulled messages per process.

Concurrency inside one process is `tokio::spawn` per message behind a `Semaphore`, with the
`Message` moved into the task so the ack happens there. Ordering is then gone; where order
matters, keep one task per consumer and scale by `filter_subject` partitions instead.

## An Err item, and what to do with it

`messages()` never yields `None` on its own, and a server restart or reconnect is transparent:
the same stream keeps delivering afterwards, with a message acked just before the outage possibly
coming back (`delivered: 2`) — at-least-once, even with `double_ack`. Trouble arrives as an
`Err` item instead:

| `MessagesErrorKind` | Meaning | After it |
|---|---|---|
| `ConsumerDeleted` | the server answered a pull with `409 Consumer Deleted` | the stream is terminated and yields `None` |
| `NoResponders` | nothing answered the pull: the consumer is gone (deleted, or lost with a memory-backed stream) | the stream stays pending, forever |
| `MissingHeartbeat` | two idle heartbeats (30 s) without the server | the stream stays pending; it may recover |
| `Pull` | a pull request itself failed | the stream stays pending |

Only `ConsumerDeleted` (and `PushBasedConsumer`) end the stream. The rest are single items
followed by silence, so a loop that logs and `continue`s hangs after a `delete_consumer`, and a
loop that propagates the error with `?` returns from the task, leaves the `JoinHandle` unpolled,
and keeps serving HTTP with its worker dead until the next deploy. The rule that survives every
case: treat the first `Err` as terminal for *this stream* — drop it, sleep with backoff, rebuild
the consumer, continue.

```rust,ignore
// Wrong: the first stream error retires this worker for the life of the process.
pub async fn run(consumer: PullConsumer) -> anyhow::Result<()> {
    let mut messages = consumer.messages().await?;
    while let Some(message) = messages.next().await {
        handle(message?).await?;
    }
    Ok(())
}
```

```rust,verify
//! The outer loop: rebuild the consumer after the first error item.
use std::time::Duration;

use async_nats::jetstream::{
    consumer::{AckPolicy, PullConsumer, pull},
    stream,
};
use tokio_stream::StreamExt as _;

/// Never returns on its own: a NATS outage, a deleted consumer and a missed
/// heartbeat are all "reopen after a pause", never "give up".
pub async fn run_forever(stream: stream::Stream, durable: String) -> ! {
    let mut backoff = Duration::from_secs(1);
    loop {
        let result = match ensure_consumer(&stream, &durable).await {
            Ok(consumer) => consume_until_error(consumer).await,
            Err(err) => Err(err),
        };
        tracing::warn!(error = ?result.err(), ?backoff, "consumer stopped; reopening");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn consume_until_error(consumer: PullConsumer) -> anyhow::Result<()> {
    let mut messages = consumer.messages().await?;
    loop {
        let message = match messages.next().await {
            Some(Ok(message)) => message,
            // `ConsumerDeleted`, `NoResponders`, `MissingHeartbeat`, `Pull`:
            // terminal for this stream, none of them terminal for the worker.
            Some(Err(err)) => {
                tracing::warn!(kind = ?err.kind(), "message stream failed");
                return Err(err.into());
            }
            None => anyhow::bail!("message stream ended"),
        };
        message.ack().await.map_err(|err| anyhow::anyhow!(err))?;
    }
}

async fn ensure_consumer(stream: &stream::Stream, durable: &str) -> anyhow::Result<PullConsumer> {
    Ok(stream
        .get_or_create_consumer(
            durable,
            pull::Config {
                durable_name: Some(durable.to_owned()),
                ack_policy: AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await?)
}

```

`ensure_consumer` is `get_or_create`, so a consumer that still exists resumes its cursor and one
that was deleted is recreated. The connection itself reconnects underneath — `max_reconnects(None)`
is the default — and a live `messages()` stream survives it without any error item at all.

## Ordered consumers

`stream.create_consumer(pull::OrderedConfig { filter_subject, .. })` builds an ephemeral consumer
(`AckPolicy::None`, `max_deliver: 1`, `max_ack_pending: 0`, memory storage, `inactive_threshold`
30 s) that detects a gap in sequence numbers and silently recreates itself from the last good
position. It is exactly one process reading in strict order,
for a read model or an in-memory cache rebuilt from the stream — never for work that must not be
lost, because there is no ack and a crash forgets the position.

## Push consumers

A push consumer (`push::Config { deliver_subject, .. }`) has the server send at its own pace to a
subject the client subscribes to. It exists for deployments that predate pull consumers and for
`deliver_group` fan-out. Its `flow_control` and `idle_heartbeat` are opt-in and per subscription
where a pull consumer gets both for free, and without them a client that falls behind is a slow
consumer whose messages are dropped and redelivered after `ack_wait`. New code does not create
one.
