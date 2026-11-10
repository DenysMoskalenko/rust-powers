# Consumers: pull, ack, redelivery, dead letters

- [Pull, durable, explicit ack](#pull-durable-explicit-ack)
- [The config, field by field](#the-config-field-by-field)
- [messages() or fetch()](#messages-or-fetch)
- [Two error types](#two-error-types)
- [Ack, nak, term, progress](#ack-nak-term-progress)
- [A dependency outage is not a retry](#a-dependency-outage-is-not-a-retry)
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
`max_deliver`, `backoff`, `max_ack_pending` in place and keeps its cursor; `deliver_policy`,
`ack_policy`, `replay_policy` and the durable name are among the fields that cannot change and
answer `err 10012` (`deliver policy can not be updated`). `create_consumer_strict` is the one that fails with `AlreadyExists`
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

const ACK_WAIT: Duration = Duration::from_secs(30);
/// One pull request. The server starts a message's `ack_wait` when it hands
/// the message over, and the stream asks for the next batch when half of this
/// one is left, so up to 1.5 batches wait behind a serial handler: keep
/// `1.5 × PULL_BATCH × slowest handler` under `ACK_WAIT` (24 × 1 s here).
const PULL_BATCH: usize = 16;

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
                // Longer than the slowest handler plus the time a message
                // waits in this process before its turn (`PULL_BATCH`).
                // Past this the server redelivers while the first attempt may
                // still be running. `backoff: vec![..]` would replace it:
                // `backoff[0]` becomes the lease, so set one or the other,
                // never both.
                ack_wait: ACK_WAIT,
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
                // The server stops delivering once this many are unacked
                // across every process on this durable: it caps what the
                // fleet holds, not what one process pulls.
                max_ack_pending: 256,
                replay_policy: ReplayPolicy::Instant,
                ..Default::default()
            },
        )
        .await?)
}

/// An endless stream that keeps its own pull requests in flight. Bare
/// `consumer.messages()` is this builder with a batch of 200 and the heartbeat
/// on; the builder itself starts with the heartbeat off, so a silent server
/// would never become a `MissingHeartbeat` item.
pub async fn open(consumer: &PullConsumer) -> anyhow::Result<pull::Stream> {
    Ok(consumer
        .stream()
        .max_messages_per_batch(PULL_BATCH)
        .heartbeat(Duration::from_secs(15))
        .messages()
        .await?)
}

/// The item is `Result<Message, MessagesError>`; the first `Err` is terminal
/// for this stream and the caller reopens (see "An Err item").
pub async fn consume(consumer: PullConsumer) -> anyhow::Result<()> {
    let mut messages = open(&consumer).await?;
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
        // A real worker copies it to `dlq.<subject>` first (the dead-letter
        // section); this shows the ack variant alone.
        return message
            .ack_with(AckKind::Term)
            .await
            .map_err(|err| anyhow::anyhow!(err));
    }
    if message.payload.starts_with(b"retry") {
        // This message cannot be handled yet and the next one can: the server
        // redelivers it after the delay, and the redelivery counts toward
        // `max_deliver`. No in-handler retry loop, it would only burn
        // `ack_wait`. A dependency that is down for every message is a
        // different case: see "A dependency outage is not a retry".
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
    // After the work is committed. `ack()` returns once the ack is queued on
    // the connection; if it is lost the message comes back after `ack_wait`
    // and the idempotent handler absorbs it.
    message.ack().await.map_err(|err| anyhow::anyhow!(err))
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
requests as the buffer drains, 200 messages per pull, with a 15 s idle heartbeat.
`consumer.stream()` builds the same stream with the numbers chosen —
`.max_messages_per_batch(n)`, `.max_bytes_per_batch(n)`, `.heartbeat(d)` — and it starts with the
heartbeat **off**, so a worker that sizes its batch sets the heartbeat too (`open` above). This
is the shape for a long-lived worker.

The batch decides whether a slow handler loses work. The server starts a message's `ack_wait`
when it hands the message over, not when the handler picks it up, and the stream asks for the
next batch once half of the last one is consumed, so up to 1.5 batches wait in the process. A
serial loop reaches the last of them after `1.5 × batch × handler time`. With the default 200 and
a 30 s `ack_wait`, a handler slower than about 100 ms lets the tail expire: the server redelivers
messages the process still holds, and every expiry counts toward `max_deliver`. Measured with a
1 s `ack_wait` and a 100 ms handler, 40 messages took 104 deliveries and some reached
`max_deliver: 5`; with a batch of 4 they took 40. Keep `1.5 × batch × slowest handler` under
`ack_wait`. The same number bounds a deploy: whatever was pulled and not handled at shutdown is
redelivered only after `ack_wait`.

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
        // Confirmed, because a job that exits after its batch can leave a
        // plain `ack()` unsent in the connection's buffer.
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
| `ack()` | done; queued on the connection, not confirmed | the default, with an idempotent handler |
| `double_ack()` | done; waits up to the context `timeout` (5 s) for the server's confirmation, one subscription and one round trip per message | a duplicate run is expensive, or the process exits right after (a batch job) |
| `ack_with(AckKind::Nak(Some(d)))` | redeliver after `d`; `None` means now; counts toward `max_deliver` | this message cannot be handled yet, the next one can |
| `ack_with(AckKind::Term)` | never redeliver; counts as handled | poison: undecodable, invalid forever |
| `ack_with(AckKind::Progress)` | reset `ack_wait`; not a delivery | a handler that legitimately outruns the lease; a message held through an outage |

Ack after the work is committed, never before: a crash between an early ack and the database
commit loses the message. Reverse the order and the same crash costs one redelivery, which the
unique key absorbs.

Neither ack makes delivery exactly-once. A crash after the commit and before the ack redelivers
under both, and so does a `double_ack` whose confirmation times out: it tells the worker that the
server has the ack, which makes a duplicate rarer and never impossible. The handler is idempotent
either way, which is why `ack()` is the default.

A `Term` on decode failure is not optional. Without it a message nobody can parse is redelivered
`max_deliver` times, or forever, and the `MAX_DELIVERIES` advisory says "unknown reason". But a
`Term` on its own fires `$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.<stream>.<consumer>`, which a
max-deliveries listener never sees: the message reaches the log and nothing else. Copy it to the
dead-letter subject first, then `Term`: the worker's `dead_letter` does both.

## A dependency outage is not a retry

`Nak(Some(delay))` is for one message that cannot be handled yet while the next one can: an order
whose customer row has not arrived, a downstream 409 that will clear. Every nak is a delivery and
`max_deliver` counts it.

A dependency every message needs being down (Postgres, the payment provider) is a different
failure, and nak-ing through it turns an outage into dead letters: with `max_deliver: 5` and a
30 s delay, two minutes of outage exhaust every message the worker touched, and it keeps pulling
fresh ones to burn. The handler returns a distinct outcome for it, and the loop does three things:

1. **Stops pulling**, by dropping the message stream. Not polling it is not enough: the stream's
   own task re-issues its pull request every 35 s whether or not anyone reads.
2. **Holds the one message in hand.** `ack_with(AckKind::Progress)` resets its `ack_wait` without
   counting a delivery; send it before each pause, keep the pause below `ack_wait`, and run the
   handler on that same message again, doubling the pause up to that cap.
3. **Reopens the stream** when that message goes through.

What the process had pulled and not reached comes back once, with `delivered: 2`, when pulling
resumes; nothing is redelivered while nobody pulls (measured: held for four `ack_wait`s, zero
redeliveries until the next pull). An outage of any length therefore costs each buffered message
one delivery, which is one more reason for a small batch and a `max_deliver` above 2.

## Redelivery and the dead-letter stream

Two paths lead to the dead-letter stream, and it captures both:

1. **Poison**, decided by the handler: the worker publishes a copy to `dlq.<original subject>`
   (`dlq.orders.placed.42`) with a durable two-await publish, then acks `Term`. The copy is a
   normal message: inspect, fix, republish to the original subject. What it carries is below.
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

One helper in the worker, `dead_letter(message, reason)`, builds the copy, and every poison path
goes through it. The copy carries:

- the original payload, untouched;
- the original's headers **except the `Nats-*` ones**. Those are instructions to the stream that
  stored the original, and they act again on the copy: a forwarded `Nats-Expected-Stream` or
  `Nats-Expected-Last-*` makes the dead-letter stream refuse it (`err 10060: expected stream does
  not match`, measured), so the message is never terminated and burns `max_deliver` instead; a
  forwarded `Nats-Msg-Id` makes the copy of a second consumer that dead-letters the same event a
  silent duplicate. `traceparent` and the application's own headers stay;
- its own `Nats-Msg-Id`, `dlq.<stream>.<consumer>.<stream sequence>`: a crash between the copy and
  the `Term` redelivers the message, and the second copy is then dropped by the dead-letter
  stream's `duplicate_window`;
- the origin, which the copy's subject alone does not give: `Dlq-Stream`, `Dlq-Consumer` and
  `Dlq-Sequence` from `message.info()`, and `Dlq-Reason`. `HeaderValue::from(&str)` panics on a
  line break, so the reason has `\r` and `\n` replaced before it becomes a header.

```rust,verify
//! The dead-letter stream: poison copies plus the max-deliveries advisory.
use async_nats::jetstream::{Context, stream};

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

Two numbers, at two levels. `max_ack_pending` is the server's: once that many messages are
delivered and unacked for the consumer — in every process bound to the durable name, queued and
in a handler alike — it stops delivering, so a slow fleet slows the stream instead of running out
of memory. It bounds what all the processes hold together; keep it at or below
`total concurrency × ack_wait ÷ handler time`, because more than that cannot be acked in time
whatever each process pulls. The pull batch (see "messages() or fetch()") is the client's and is
per process, and it is the one a serial worker trips first: bare `messages()` asks for up to 300
at once, and `max_ack_pending: 256` lets 256 of them through.

Concurrency inside one process is `tokio::spawn` per message behind a `Semaphore`, with the
`Message` moved into the task so the ack happens there. Ordering is then gone; where order
matters, keep one task per consumer and scale by `filter_subject` partitions instead.

## An Err item, and what to do with it

`messages()` never yields `None` on its own. A server restart or a reconnect shorter than two
idle heartbeats (30 s) is transparent: the same stream keeps delivering afterwards, with a message
acked just before the outage possibly coming back (`delivered: 2`) — at-least-once, even with
`double_ack`. A longer one yields `MissingHeartbeat`. Trouble arrives as an `Err` item:

| `MessagesErrorKind` | Meaning | After it |
|---|---|---|
| `ConsumerDeleted` | the server answered a pull with `409 Consumer Deleted` | the stream is terminated and yields `None` |
| `NoResponders` | nothing answered the pull: the consumer is gone (deleted, or lost with a memory-backed stream) | the stream stays pending, forever |
| `MissingHeartbeat` | two idle heartbeats (30 s) without the server, an outage that long included; never yielded when the heartbeat is off | the stream stays pending; it may recover |
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
use tokio::time::Instant;
use tokio_stream::StreamExt as _;

const FIRST_PAUSE: Duration = Duration::from_secs(1);
/// A stream that stayed open this long had recovered, so the failure that
/// ends it is a new one: the pause starts again from `FIRST_PAUSE` instead of
/// staying at the cap for the life of the process.
const HEALTHY: Duration = Duration::from_secs(60);

/// Never returns on its own: a NATS outage, a deleted consumer and a missed
/// heartbeat are all "reopen after a pause", never "give up".
pub async fn run_forever(stream: stream::Stream, durable: String) -> ! {
    let mut backoff = FIRST_PAUSE;
    loop {
        let opened = Instant::now();
        let result = match ensure_consumer(&stream, &durable).await {
            Ok(consumer) => consume_until_error(consumer).await,
            Err(err) => Err(err),
        };
        if opened.elapsed() > HEALTHY {
            backoff = FIRST_PAUSE;
        }
        tracing::warn!(error = ?result.err(), ?backoff, "consumer stopped; reopening");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn consume_until_error(consumer: PullConsumer) -> anyhow::Result<()> {
    let mut messages = consumer
        .stream()
        .max_messages_per_batch(16)
        .heartbeat(Duration::from_secs(15))
        .messages()
        .await?;
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
is the default — and a live stream survives a reconnect shorter than two heartbeats without any
error item at all; a longer one arrives here as `MissingHeartbeat`.

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
