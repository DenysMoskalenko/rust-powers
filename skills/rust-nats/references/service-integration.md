# Wiring NATS into the service

- [State: the client is Clone](#state-the-client-is-clone)
- [Settings](#settings)
- [Publishing from a handler](#publishing-from-a-handler)
- [Mapping onto AppError](#mapping-onto-apperror)
- [The worker task](#the-worker-task)
- [Idempotent handlers](#idempotent-handlers)
- [Process wiring, startup retry and shutdown order](#process-wiring-startup-retry-and-shutdown-order)
- [Readiness](#readiness)
- [Telemetry](#telemetry)
- [Easy to get wrong](#easy-to-get-wrong)

## State: the client is Clone

`async_nats::Client` is a handle over one multiplexed connection with a background task reading
the socket; cloning it clones an `Arc` and a channel sender. `jetstream::Context` is the same
handle plus a prefix. Both go into `AppState` by value, never behind another `Arc`, and every task
that needs one clones it. One connection per process is the norm; a second one buys nothing.

```rust
#[derive(Clone)]
pub struct AppState {
    pub db: sea_orm::DatabaseConnection,
    // ...
    pub nats: async_nats::Client,
    pub jetstream: async_nats::jetstream::Context,
}
```

`AppState`, `build_router`, the health routes and `axum::serve` belong to `axum-service`; this
file adds the fields, the startup step and the shutdown step. The fields are an addition, not a
requirement: a worker that only consumes takes its `jetstream::Context` from `main` and touches
neither `AppState` nor `test_app()`. Only a handler that publishes needs them in state.

## Settings

A `MessagingSettings { url: SecretString, name: String, stream: String }` section beside the
existing ones, read as `APP__MESSAGING__URL`. The URL is a secret because a token or a `.creds`
path rides in it. Local default `nats://localhost:4222`; the compose service (`nats:2.12-alpine`
with `-js`) and the CI `services:` entry are in `rust-tooling`'s optional-services block.

## Publishing from a handler

Core `publish` returns once the bytes are queued on the local connection: a subscriber may not
exist, the server may not have them yet. JetStream `send_publish` returns the stream's ack after
the **second** await, and `message_id` makes a retry of the same event a no-op. Request-reply
gets a per-call timeout well inside the HTTP budget; the client default is 10 s, which is the
whole outbound-HTTP budget of `axum-service`.

```rust,verify
//! Publish, request, respond — and the one error type they share.
use std::time::Duration;

use async_nats::{
    HeaderMap, RequestErrorKind,
    jetstream::{Context, message::PublishMessage},
};
use tokio_stream::StreamExt as _;

use crate::error::AppError;

/// Every NATS failure a handler can see. `AppError` gets no variant and no
/// `From` for it: a handler maps at the call site with `messaging_error`.
#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    /// Nobody is subscribed (`no responders`), or nobody answered in time
    /// (`timed out`: a slow responder, or NATS itself unreachable).
    #[error(transparent)]
    Request(#[from] async_nats::RequestError),
    /// The local connection did not accept the message.
    #[error(transparent)]
    Publish(#[from] async_nats::PublishError),
    #[error(transparent)]
    Subscribe(#[from] async_nats::SubscribeError),
    /// The stream refused the message or never acked it.
    #[error(transparent)]
    JetStream(#[from] async_nats::jetstream::context::PublishError),
}

/// Onto the canonical variants only. All four errors are `Error<Kind>`
/// structs: match on `kind()`, never on the error itself.
pub fn messaging_error(err: MessagingError) -> AppError {
    use async_nats::jetstream::context::PublishErrorKind as J;
    use async_nats::{PublishErrorKind as P, SubscribeErrorKind as S};
    // (refused client-side before sending, the bus is unreachable or silent);
    // anything else is a wiring or programming error.
    let (bad_input, unavailable) = match &err {
        MessagingError::Request(e) => (
            matches!(e.kind(), RequestErrorKind::InvalidSubject | RequestErrorKind::MaxPayloadExceeded),
            matches!(e.kind(), RequestErrorKind::NoResponders | RequestErrorKind::TimedOut),
        ),
        MessagingError::Publish(e) => (
            matches!(e.kind(), P::InvalidSubject | P::MaxPayloadExceeded),
            e.kind() == P::Send,
        ),
        MessagingError::Subscribe(e) => (false, e.kind() == S::Other),
        MessagingError::JetStream(e) => (
            e.kind() == J::MaxPayloadExceeded,
            matches!(e.kind(), J::StreamNotFound | J::TimedOut | J::BrokenPipe | J::MaxAckPending),
        ),
    };
    if bad_input {
        // 400: rendered to the client, so a constant, not the error's display.
        AppError::BadRequest("message payload too large or subject invalid".to_owned())
    } else if unavailable {
        // 503 with a constant body; `no responders: ..` or `request timed out:
        // deadline has elapsed` reaches the log only.
        AppError::Unavailable(format!("nats: {err}"))
    } else {
        // 500: `WrongLastSequence`, an invalid queue name, `Other`.
        AppError::Other(err.into())
    }
}

/// Core NATS, fire and forget. Success means "queued locally".
pub async fn notify(
    client: &async_nats::Client,
    subject: String,
    payload: Vec<u8>,
) -> Result<(), MessagingError> {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json");
    client
        .publish_with_headers(subject, headers, payload.into())
        .await?;
    Ok(())
}

/// `JetStream`: durable once the second await returns. `message_id` is the
/// event's own id, minted when the event was created, so a retried publish
/// is deduplicated by the server.
pub async fn publish_durable(
    ctx: &Context,
    subject: String,
    message_id: &str,
    payload: Vec<u8>,
) -> Result<u64, MessagingError> {
    let ack = ctx
        .send_publish(
            subject,
            PublishMessage::build()
                .payload(payload.into())
                .message_id(message_id),
        )
        .await?
        .await?;
    Ok(ack.sequence)
}

/// Request-reply. `no responders` comes back at once when nothing is
/// subscribed and the server is connected; while disconnected the request
/// waits out the whole timeout and fails with `TimedOut`.
pub async fn ask_pricing(client: &async_nats::Client, sku: &str) -> Result<String, MessagingError> {
    let request = async_nats::Request::new()
        .payload(sku.to_owned().into())
        .timeout(Some(Duration::from_millis(500)));
    let response = client.send_request("pricing.quote", request).await?;
    Ok(String::from_utf8_lossy(&response.payload).into_owned())
}

/// The responder. There is no `Message::respond` in core NATS: publish to
/// `message.reply`, and skip a message without one (it came from `publish`,
/// not `request`). A queue group makes every replica share the load.
pub async fn respond_pricing(client: async_nats::Client) -> Result<(), MessagingError> {
    let mut requests = client
        .queue_subscribe("pricing.quote", "pricing".to_owned())
        .await?;
    while let Some(message) = requests.next().await {
        let Some(reply) = message.reply else {
            tracing::warn!("request without reply subject; dropping");
            continue;
        };
        client.publish(reply, "4200".into()).await?;
    }
    Ok(())
}

```

`flush()` is the only way to know the server received buffered core publishes. It belongs at
shutdown and before a test asserts, not after every message.

## Mapping onto AppError

`AppError` is `axum-service`'s and gains nothing here — no `Messaging` variant, no
`From<MessagingError>`. A handler writes `.map_err(messaging_error)?`, so every NATS failure is a
visible decision at the call site, and the mapping lands only on canonical variants:

| Kind | `AppError` | Why |
|---|---|---|
| `RequestErrorKind::NoResponders`, `TimedOut`; `PublishErrorKind::Send`; JetStream `StreamNotFound`, `TimedOut`, `BrokenPipe`, `MaxAckPending`; `SubscribeErrorKind::Other` | `Unavailable(String)` — 503, body `service unavailable`, the string logged | nothing is subscribed, no stream captures the subject, or the socket is gone; the client may retry later |
| `MaxPayloadExceeded`, `InvalidSubject` (core or JetStream) | `BadRequest(String)` — 400 | refused client-side before anything was sent |
| `WrongLastSequence`, `WrongLastMessageId`, subscribe-side `InvalidSubject`/`InvalidQueueName`, `Other` | `Other(anyhow::Error)` — 500 | a failed optimistic-concurrency expectation or a wiring error: subscribe subjects are never client input |

`TimedOut` is deliberately not 504: a disconnected client returns it too, so it does not mean "a
responder exists and is slow". Nothing maps to 502; that status is the `Http` variant for outbound
HTTP and stays there.

```rust
// In a handler: the map is the only place NATS meets HTTP.
let sequence = publish_durable(&state.jetstream, subject, &event.event_id.to_string(), body)
    .await
    .map_err(messaging_error)?;
```

## The worker task

The consumer runs as one tokio task per durable consumer, owned by `main`, stopped through a
`tokio::sync::watch` channel. Handlers are plain functions from a typed event to an `Outcome`;
the loop does the acking, so a handler is unit-tested with a struct literal and no broker.

```rust,verify
//! The consumer task: typed envelope, handler per subject, shutdown by watch.
use std::time::Duration;

use async_nats::jetstream::{
    AckKind, Message,
    consumer::{AckPolicy, PullConsumer, pull},
    message::PublishMessage,
    stream,
};
use tokio::{sync::watch, time::timeout};
use tokio_stream::StreamExt as _;
use uuid::Uuid;

/// One envelope type per subject family. `event_id` is the `Nats-Msg-Id` on
/// publish and the unique key on the consumer side.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrderPlaced {
    pub event_id: Uuid,
    pub order_id: Uuid,
    pub total_cents: i64,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}

impl OrderPlaced {
    /// General to specific, lowercase, the id last: a consumer filters with
    /// `orders.placed.*`, a metric labels by the family `orders.placed`.
    pub fn subject(&self) -> String {
        format!("orders.placed.{}", self.order_id)
    }
}

/// What a handler decided. The loop acks; the handler never sees NATS.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Transient: redeliver after this delay.
    Retry(Duration),
    /// Permanent: stop redelivery, the message is poison.
    Poison(String),
}

pub fn handle_order_placed(event: &OrderPlaced) -> Outcome {
    if event.total_cents < 0 {
        return Outcome::Poison("negative total".to_owned());
    }
    Outcome::Done
}

/// Handler per subject family. A decode failure is poison, never a retry:
/// the bytes will not parse better the fifth time.
pub fn dispatch(message: &Message) -> Outcome {
    let subject = message.subject.as_str();
    if subject.starts_with("orders.placed.") {
        return match serde_json::from_slice::<OrderPlaced>(&message.payload) {
            Ok(event) => handle_order_placed(&event),
            Err(err) => Outcome::Poison(format!("undecodable OrderPlaced: {err}")),
        };
    }
    Outcome::Poison(format!("no handler for {subject}"))
}

/// Runs until `stop` flips to true or its sender is dropped. An error from
/// the message stream is not a reason to return: the consumer is rebuilt
/// after a pause and the loop continues, or the process would keep serving
/// HTTP with a dead worker.
pub async fn run(stream: stream::Stream, durable: String, mut stop: watch::Receiver<bool>) {
    let mut backoff = Duration::from_secs(1);
    while !*stop.borrow() {
        let result = match ensure_consumer(&stream, &durable).await {
            Ok(consumer) => consume(consumer, &mut stop).await,
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            tracing::error!(%err, ?backoff, "consumer stopped; reopening");
            // The pause ends early on shutdown: a plain `sleep` would hold the
            // join for up to 30 s. `Ok` means the watch changed or was dropped.
            if timeout(backoff, stop.changed()).await.is_ok() {
                return;
            }
            // ponytail: never resets; 30 s is an acceptable worst case.
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

async fn consume(consumer: PullConsumer, stop: &mut watch::Receiver<bool>) -> anyhow::Result<()> {
    let mut messages = consumer.messages().await?;
    loop {
        let next = tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            next = messages.next() => next,
        };
        let message = match next {
            Some(Ok(message)) => message,
            // The first `Err` is terminal for this stream (`ConsumerDeleted`,
            // `NoResponders`, `MissingHeartbeat`, `Pull`): after it the stream
            // stays pending, so return and let `run` rebuild the consumer.
            Some(Err(err)) => return Err(err.into()),
            None => anyhow::bail!("message stream ended"),
        };
        if let Err(err) = settle(&message, dispatch(&message)).await {
            tracing::error!(%err, "ack failed; the message will be redelivered");
        }
    }
}

/// The only place that acks. `double_ack` waits for the server, so a lost ack
/// cannot cause a redelivery the handler cannot absorb.
async fn settle(message: &Message, outcome: Outcome) -> Result<(), async_nats::Error> {
    match outcome {
        Outcome::Done => message.double_ack().await,
        Outcome::Retry(delay) => message.ack_with(AckKind::Nak(Some(delay))).await,
        Outcome::Poison(reason) => {
            tracing::warn!(%reason, subject = %message.subject, "dead-lettering message");
            dead_letter(message, &reason).await
        }
    }
}

/// Copy to `dlq.<subject>` first, then `Term`. `Term` alone fires only a
/// `MSG_TERMINATED` advisory, which the dead-letter stream does not capture.
async fn dead_letter(message: &Message, reason: &str) -> Result<(), async_nats::Error> {
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

async fn ensure_consumer(stream: &stream::Stream, durable: &str) -> anyhow::Result<PullConsumer> {
    Ok(stream
        .get_or_create_consumer(
            durable,
            pull::Config {
                durable_name: Some(durable.to_owned()),
                ack_policy: AckPolicy::Explicit,
                ack_wait: Duration::from_secs(30),
                max_deliver: 5,
                filter_subject: "orders.placed.*".to_owned(),
                max_ack_pending: 256,
                ..Default::default()
            },
        )
        .await?)
}

```

Per-message concurrency is `tokio::spawn` behind a `Semaphore` with the `Message` moved into
the task; it gives up ordering, so partition by `filter_subject` and run one task per partition
where order matters.

## Idempotent handlers

`Nats-Msg-Id` deduplicates the *publish* inside `duplicate_window`. A consumer that did the work
and crashed before the ack sees the message again, and so does one whose `double_ack` was lost.
The handler is therefore idempotent by construction: the event id is a unique column in the
table the handler writes, the insert runs in the same transaction as the work, and a
`SqlErr::UniqueConstraintViolation` on it is treated as `Outcome::Done`. Ack after the commit.
The Postgres side is `sea-orm-postgres`.

## Process wiring, startup retry and shutdown order

`main` calls `start` after settings and telemetry, puts `messaging.bus` into `AppState`, runs
`axum::serve(..).with_graceful_shutdown(..)` (that call, the router and the readiness route are
`axum-service`'s), and calls `messaging.shutdown()` once it returns.

```rust,verify
//! One client, one `JetStream` context, one worker task; startup that retries
//! and a shutdown whose every wait is bounded.
use std::time::Duration;

use async_nats::{
    ConnectOptions, Event,
    connection::State,
    jetstream::{self, consumer::pull, stream},
};
use tokio::{sync::watch, time::timeout};
use tokio_stream::StreamExt as _;

use crate::api::health::Health;

/// What `AppState` gains. Cheap handles over the one connection.
#[derive(Clone)]
pub struct Bus {
    pub nats: async_nats::Client,
    pub jetstream: jetstream::Context,
}

/// Everything `main` holds: the state fields and the handle that ends the worker.
pub struct Messaging {
    pub bus: Bus,
    stop: watch::Sender<bool>,
    worker: tokio::task::JoinHandle<()>,
}

/// The four settings that are not defaults and must be set.
pub async fn connect(url: &str) -> Result<async_nats::Client, async_nats::ConnectError> {
    ConnectOptions::new()
        // Shows in `nats server report connections`; unset is "unnamed".
        .name("orders-api")
        // The default 10 s is the whole outbound-HTTP budget. This also bounds
        // every JetStream API call: stream info, consumer create, `fetch`.
        .request_timeout(Some(Duration::from_secs(2)))
        // Off by default: without it a NATS that boots second crash-loops the
        // pod. With it `connect()` returns `Ok` while still `Pending` and the
        // socket comes up in the background, so the first JetStream call may
        // time out: `declare_stream` retries.
        .retry_on_initial_connect()
        // Reconnects are infinite by default; this is where they get logged.
        .event_callback(|event| async move {
            match event {
                Event::Connected => tracing::info!("nats connected"),
                Event::Disconnected => tracing::warn!("nats disconnected"),
                Event::SlowConsumer(sid) => tracing::warn!(sid, "nats slow consumer"),
                other => tracing::info!(%other, "nats event"),
            }
        })
        .connect(url)
        .await
}

/// The `"messaging"` entry for `axum-service`'s readiness `checks` map: the
/// client's cached view, no round trip. NATS is optional, so a lost connection
/// is `Degraded` (the probe stays 200), never `Unavailable`.
pub fn readiness(client: &async_nats::Client) -> Health {
    match client.connection_state() {
        State::Connected => Health::Ok,
        _ => Health::Degraded,
    }
}

/// `get_or_create_stream` with backoff: `TimedOut` here usually means the
/// connection is still pending, not that `JetStream` is broken.
async fn declare_stream(ctx: &jetstream::Context) -> anyhow::Result<stream::Stream> {
    let mut delay = Duration::from_secs(1);
    loop {
        let config = stream::Config {
            name: "ORDERS".to_owned(),
            subjects: vec!["orders.>".to_owned()],
            ..Default::default()
        };
        match ctx.get_or_create_stream(config).await {
            Ok(stream) => return Ok(stream),
            Err(err) if delay < Duration::from_secs(30) => {
                tracing::warn!(%err, ?delay, "stream declaration failed; retrying");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

pub async fn start(nats_url: &str) -> anyhow::Result<Messaging> {
    let client = connect(nats_url).await?;
    let jetstream = jetstream::new(client.clone());
    let stream = declare_stream(&jetstream).await?;
    let (stop, stopped) = watch::channel(false);
    let worker = tokio::spawn(worker(stream, stopped));
    Ok(Messaging {
        bus: Bus {
            nats: client,
            jetstream,
        },
        stop,
        worker,
    })
}

/// The skeleton of "The worker task": the first `Err` item is terminal for
/// the message stream, never for the task.
async fn worker(stream: stream::Stream, mut stop: watch::Receiver<bool>) {
    let mut backoff = Duration::from_secs(1);
    while !*stop.borrow() {
        if let Err(err) = consume(&stream, &mut stop).await {
            tracing::error!(%err, ?backoff, "consumer stopped; reopening");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

async fn consume(stream: &stream::Stream, stop: &mut watch::Receiver<bool>) -> anyhow::Result<()> {
    let consumer = stream
        .get_or_create_consumer(
            "orders-worker",
            pull::Config {
                durable_name: Some("orders-worker".to_owned()),
                ..Default::default()
            },
        )
        .await?;
    let mut messages = consumer.messages().await?;
    loop {
        let next = tokio::select! {
            biased;
            _ = stop.changed() => return Ok(()),
            next = messages.next() => next,
        };
        let message = match next {
            Some(Ok(message)) => message,
            Some(Err(err)) => return Err(err.into()),
            None => anyhow::bail!("message stream ended"),
        };
        message.ack().await.map_err(|err| anyhow::anyhow!(err))?;
    }
}

impl Messaging {
    /// Runs after HTTP has drained. Every wait is bounded and none of them
    /// aborts the rest: a worker that will not stop is abandoned, `flush()`
    /// blocks for as long as NATS is down (still pending after 12 s when
    /// measured), and `drain()` returns before draining ends and leaves the
    /// client unusable, so it is last.
    pub async fn shutdown(self) {
        self.stop.send_replace(true);
        bounded("worker join", Duration::from_secs(10), self.worker).await;
        bounded("flush", Duration::from_secs(2), self.bus.nats.flush()).await;
        bounded("drain", Duration::from_secs(2), self.bus.nats.drain()).await;
    }
}

/// One shutdown step: logged, never propagated, never unbounded.
async fn bounded<E: std::fmt::Display>(
    step: &str,
    limit: Duration,
    fut: impl Future<Output = Result<(), E>>,
) {
    let failure = match timeout(limit, fut).await {
        Ok(Ok(())) => return,
        Ok(Err(err)) => err.to_string(),
        Err(_) => format!("did not finish within {limit:?}"),
    };
    tracing::warn!(step, %failure, "shutdown step failed");
}

```

The service's real order is: HTTP drained → worker stopped → `flush` → `drain` → exporter
flushed → pool dropped. The last two are `axum-service`'s. A worker that returns early skips
nothing: `flush` and `drain` still run, each under its own timeout. Up to 200 pre-pulled
messages the worker had not reached are redelivered after `ack_wait`.

## Readiness

`GET /health/ready`, its body `{ "status", "checks": { "<name>": .. } }` and the rule that only a
required dependency turns the probe into a 503 are `axum-service`'s. This skill may add one entry
to the `checks` map: `"messaging"`, from `readiness(&state.nats)` above — optional, and only once
the client is in `AppState`. Adding it changes the `/health/ready` body, which `tests/health.rs`
asserts verbatim, so that test changes in the same commit. It reads
`client.connection_state()` — local, synchronous, no round trip — because a probe that does a round
trip turns a NATS hiccup into every replica leaving rotation at once. With
`retry_on_initial_connect()` the state is `Pending` until the socket is up, so a pod that started
before NATS reports `"messaging": "degraded"` instead of crashing, and stays in rotation: a
service that can still serve reads and fire-and-forget notifications is degraded, not down. Only
a service that cannot do useful work without the bus promotes the entry to `Unavailable`, and
that is a deliberate per-service decision written next to the check.

## Telemetry

The producer span wraps the publish and injects the current context into the NATS headers, so a
`traceparent` rides with the message exactly as it does with an HTTP request. The consumer span
extracts it and sets it as parent, and the collector shows the HTTP request that placed the order
and the worker that processed it as one trace. Field names follow the OTel messaging semantic
conventions so NATS spans group with everything else's: the span name is
`{messaging.operation.name} {messaging.destination.template}` — `publish orders.placed.{order_id}`
— because a span name must be low-cardinality and the concrete subject carries an id;
`messaging.destination.name` holds the concrete subject and `messaging.operation.type` the
semconv category (`send`, `process`).

`global::set_text_map_propagator(TraceContextPropagator::new())` must already have run in the
telemetry init — the same line the HTTP side needs. The default global propagator is a no-op
that injects nothing and warns about nothing.

```rust,verify
//! Trace context through NATS headers, the message spans, and counters.
use opentelemetry::{
    Context,
    propagation::{Extractor, Injector},
};
use tracing::{Instrument as _, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

pub struct HeaderInjector<'a>(pub &'a mut async_nats::HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key, value.as_str());
    }
}

pub struct HeaderExtractor<'a>(pub &'a async_nats::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(async_nats::HeaderValue::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0.iter().map(|(name, _)| name.as_ref()).collect()
    }
}

/// Writes `traceparent` (and `tracestate`) for the current span.
pub fn inject_current(headers: &mut async_nats::HeaderMap) {
    let cx = Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&cx, &mut HeaderInjector(headers));
    });
}

/// `template` is the subject family with its placeholder, `orders.placed.{order_id}`:
/// it names the span, so it must be low-cardinality; the concrete subject is a field.
pub fn producer_span(template: &'static str, subject: &str) -> Span {
    tracing::info_span!(
        "nats publish",
        otel.kind = "producer",
        otel.name = %format!("publish {template}"),
        messaging.system = "nats",
        messaging.operation.name = "publish",
        messaging.operation.type = "send",
        messaging.destination.template = template,
        messaging.destination.name = %subject,
    )
}

/// Parented to the producer's trace when the message carries one. The worker
/// knows its `filter_subject`, so it knows the template.
pub fn consumer_span(template: &'static str, message: &async_nats::Message) -> Span {
    let span = tracing::info_span!(
        "nats consume",
        otel.kind = "consumer",
        otel.name = %format!("process {template}"),
        messaging.system = "nats",
        messaging.operation.name = "process",
        messaging.operation.type = "process",
        messaging.destination.template = template,
        messaging.destination.name = %message.subject,
        messaging.message.body.size = message.payload.len(),
    );
    if let Some(headers) = message.headers.as_ref() {
        let parent: Context = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeaderExtractor(headers))
        });
        // `set_parent` returns a `Result` in tracing-opentelemetry 0.33 and
        // `unused_must_use = "deny"` rejects a bare call.
        if let Err(err) = span.set_parent(parent) {
            tracing::debug!(%err, "no usable trace context on message");
        }
    }
    span
}

/// Inject *inside* the producer span, or the headers carry the caller's
/// context instead of the publish span's.
pub async fn publish_traced(
    client: &async_nats::Client,
    subject: String,
    payload: Vec<u8>,
) -> Result<(), async_nats::PublishError> {
    let span = producer_span("orders.placed.{order_id}", &subject);
    async {
        let mut headers = async_nats::HeaderMap::new();
        inject_current(&mut headers);
        client
            .publish_with_headers(subject, headers, payload.into())
            .await?;
        // Label by subject family, never by the full subject: an id in the
        // subject is unbounded cardinality.
        metrics::counter!("nats_messages_published_total", "family" => "orders.placed")
            .increment(1);
        Ok(())
    }
    .instrument(span)
    .await
}

/// The consumer side: one span per message, counters after the ack.
pub async fn process_traced(message: &async_nats::jetstream::Message) {
    let span = consumer_span("orders.placed.{order_id}", message);
    async {
        let started = std::time::Instant::now();
        if let Err(err) = message.ack().await {
            tracing::error!(%err, "ack failed");
        }
        metrics::counter!("nats_messages_consumed_total", "family" => "orders.placed").increment(1);
        metrics::histogram!("nats_message_handle_seconds", "family" => "orders.placed")
            .record(started.elapsed().as_secs_f64());
    }
    .instrument(span)
    .await;
}

```

Spans carry the subject and the payload size, never the payload: it is user data at info level.
Counters: `nats_messages_published_total`, `nats_messages_consumed_total`, plus a handle-time
histogram, all labelled by subject family.

## Easy to get wrong

Only the calls whose name does not give them away; the rest reads as it is spelled.

| Need | async-nats |
|---|---|
| connection lifecycle callbacks | one `event_callback(\|event\| async move { .. })` |
| retry the first connect | `.retry_on_initial_connect()` — off by default |
| reply to a request | `client.publish(msg.reply.unwrap(), data)` — there is no `respond` |
| publish with a dedup id | `js.send_publish(subj, PublishMessage::build().message_id(id)).await?.await?` |
| ack synchronously, or nak with a delay | `double_ack()`, `ack_with(AckKind::Nak(Some(5s)))` |
| extend the ack deadline | `ack_with(AckKind::Progress)` — there is no `in_progress()` |
| expose a micro service | `client.service_builder()` with `ServiceExt` imported |
