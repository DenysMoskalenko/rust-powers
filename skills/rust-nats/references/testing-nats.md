# Testing against a real NATS

- [What needs a broker](#what-needs-a-broker)
- [The nextest process model](#the-nextest-process-model)
- [The container, its tag, and two races](#the-container-its-tag-and-two-races)
- [Isolation: a per-test prefix](#isolation-a-per-test-prefix)
- [The harness and the tests](#the-harness-and-the-tests)
- [What is worth a test](#what-is-worth-a-test)
- [Compose and CI](#compose-and-ci)

## What needs a broker

A handler is a plain function from a typed event to an `Outcome`, so most consumer logic is a
unit test with a struct literal and no NATS at all. Only the wiring needs a server: that a
duplicate `Nats-Msg-Id` is dropped, that a nak redelivers and a term does not, that a poison
message's copy and the max-deliveries advisory both land in the dead-letter stream, that `no
responders` comes back before the timeout, that a queue group splits work, that `traceparent`
survives the headers. None of that
exists in a fake, and the crate ships none — which is right, because a fake of an at-least-once
broker that never redelivers is a test that passes for the wrong reason.

`TEST_NATS_URL` short-circuits the container, exactly as `TEST_DATABASE_URL` does for Postgres;
the general test harness is `rust-testing`'s.

## The nextest process model

cargo-nextest runs every test in its own process, so a `static`/`OnceLock` container is per
*test*, not per binary. Two arrangements survive that: an external server named by
`TEST_NATS_URL` (compose locally, a `services:` container in CI), or one container reused by name
with `ReuseDirective::Always`, where the first process to start it wins and every later process
attaches. Reused containers are never cleaned up; that is the point.

## The container, its tag, and two races

`testcontainers-modules` 0.15 has a `nats` feature. Its default image is `nats:2.10.14`, two
minors behind; pin `.with_tag("2.12-alpine")`. JetStream is off unless
`NatsServerCmd::default().with_jetstream()` is passed through `.with_cmd(&cmd)` — by reference,
because the command type implements `IntoIterator` for `&Self`. Readiness waits for
`Server is ready` on stderr.

Two races, both fixed by retrying:

1. Every nextest process races to *create* the one named container and all but the winner get a
   Docker `409 Conflict: container name is already in use`. Retrying `start()` attaches to the
   winner's container.
2. Docker's port proxy accepts TCP before `nats-server` inside a freshly created container is
   listening, so an attacher can see `expected INFO, got nothing` on connect. Retry the connect.

nextest may mark a NATS test `LEAK`: the reused container handle and a subscriber task outlive
the test body. It is a pass, not a failure.

## Isolation: a per-test prefix

Every test gets a uuid v7 prefix and puts it on every subject, stream name, consumer name and
bucket. Subjects need no cleanup; streams and buckets are deleted at the end of the test. This
works against a shared container, a compose service and CI alike, and survives full parallelism.
Two tests sharing a stream name see each other's messages and each other's consumer cursors.

Every wait on a stream is wrapped in `tokio::time::timeout`: a bare `.next().await` on a
subscription that never yields hangs the whole run.

## The harness and the tests

In a real suite the harness is `tests/common/mod.rs` and every test binary opens with
`mod common;`. It is shown inline so the file is complete.

```rust,verify,test
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "allow-unwrap-in-tests covers #[test] bodies, not helpers"
)]

use std::time::Duration;

use async_nats::RequestErrorKind;
use async_nats::jetstream::{
    AckKind, Message,
    consumer::{AckPolicy, DeliverPolicy, PullConsumer, pull, pull::MessagesErrorKind},
    message::PublishMessage,
    stream,
};
use testcontainers::{ContainerAsync, ImageExt as _, ReuseDirective, runners::AsyncRunner};
use testcontainers_modules::nats::{Nats, NatsServerCmd};
use tokio_stream::StreamExt as _;
use uuid::Uuid;

pub struct TestNats {
    pub client: async_nats::Client,
    pub jetstream: async_nats::jetstream::Context,
    /// On every subject, stream, consumer and bucket this test touches.
    pub prefix: String,
    _container: Option<ContainerAsync<Nats>>,
}

impl TestNats {
    pub fn subject(&self, suffix: &str) -> String {
        format!("{}.{suffix}", self.prefix)
    }

    /// Stream names may not contain dots; the prefix has none.
    pub fn stream_name(&self) -> String {
        format!("S{}", self.prefix.to_uppercase())
    }

    pub async fn stream(&self, root: &str) -> stream::Stream {
        self.jetstream
            .get_or_create_stream(stream::Config {
                name: self.stream_name(),
                subjects: vec![format!("{root}.>")],
                storage: stream::StorageType::Memory,
                duplicate_window: Duration::from_secs(120),
                ..Default::default()
            })
            .await
            .expect("create stream")
    }

    pub async fn cleanup(&self) {
        self.jetstream
            .delete_stream(self.stream_name())
            .await
            .expect("delete stream");
    }
}

/// `TEST_NATS_URL` wins; otherwise a reusable container with `JetStream` on.
pub async fn test_nats() -> TestNats {
    let prefix = format!("t{}", Uuid::now_v7().simple());
    if let Ok(url) = std::env::var("TEST_NATS_URL") {
        let client = connect(&url).await;
        let jetstream = async_nats::jetstream::new(client.clone());
        return TestNats {
            client,
            jetstream,
            prefix,
            _container: None,
        };
    }
    let container = start_reusable().await;
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(4222)
        .await
        .expect("container port");
    let client = connect(&format!("nats://{host}:{port}")).await;
    let jetstream = async_nats::jetstream::new(client.clone());
    TestNats {
        client,
        jetstream,
        prefix,
        _container: Some(container),
    }
}

/// Every nextest process races to create the one named container; the losers
/// get a Docker 409 and find the winner's container on retry.
async fn start_reusable() -> ContainerAsync<Nats> {
    let cmd = NatsServerCmd::default().with_jetstream();
    let mut last = String::new();
    for attempt in 0..30_u32 {
        let started = Nats::default()
            .with_cmd(&cmd)
            // The module default is 2.10.14. Pin what production runs.
            .with_tag("2.12-alpine")
            .with_container_name("rust-powers-test-nats")
            .with_reuse(ReuseDirective::Always)
            .start()
            .await;
        match started {
            Ok(container) => return container,
            Err(err) => {
                last = err.to_string();
                tokio::time::sleep(Duration::from_millis(200 + u64::from(attempt) * 100)).await;
            }
        }
    }
    panic!("could not start or reuse the nats container: {last}")
}

/// Docker's port proxy answers before the server inside does, so an attacher
/// can get `expected INFO, got nothing`. Retry instead of failing the test.
async fn connect(url: &str) -> async_nats::Client {
    let mut last = String::new();
    for _ in 0..40_u32 {
        match async_nats::ConnectOptions::new()
            .request_timeout(Some(Duration::from_secs(5)))
            .connect(url)
            .await
        {
            Ok(client) => return client,
            Err(err) => {
                last = err.to_string();
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
    panic!("could not connect to {url}: {last}")
}

fn pull_config(durable: &str, filter: String) -> pull::Config {
    pull::Config {
        durable_name: Some(durable.to_owned()),
        ack_policy: AckPolicy::Explicit,
        ack_wait: Duration::from_secs(1),
        max_deliver: 2,
        filter_subject: filter,
        deliver_policy: DeliverPolicy::All,
        ..Default::default()
    }
}

async fn fetch_one(consumer: &PullConsumer) -> Option<Message> {
    consumer
        .fetch()
        .max_messages(1)
        .expires(Duration::from_secs(3))
        .messages()
        .await
        .expect("fetch")
        .next()
        .await
        .map(|message| message.expect("no stream error"))
}

/// The worker's poison path: copy to `dlq.<subject>`, then `Term`.
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

#[tokio::test]
async fn same_message_id_is_deduplicated_and_delivered_once() {
    let t = test_nats().await;
    let root = t.subject("orders");
    let stream = t.stream(&root).await;
    let subject = format!("{root}.placed.1");
    let event_id = Uuid::now_v7().to_string();

    let publish = async || {
        t.jetstream
            .send_publish(
                subject.clone(),
                PublishMessage::build()
                    .payload(b"{}".to_vec().into())
                    .message_id(event_id.clone()),
            )
            .await
            .expect("publish")
            .await
            .expect("ack")
    };
    let first = publish().await;
    let second = publish().await;

    assert!(!first.duplicate);
    assert!(second.duplicate, "same Nats-Msg-Id inside duplicate_window");
    assert_eq!(
        first.sequence, second.sequence,
        "the original sequence comes back"
    );
    assert_eq!(stream.get_info().await.unwrap().state.messages, 1);

    let consumer = stream
        .get_or_create_consumer("worker", pull_config("worker", format!("{root}.placed.*")))
        .await
        .unwrap();
    let message = fetch_one(&consumer).await.expect("one message");
    assert_eq!(message.info().unwrap().delivered, 1);
    message.double_ack().await.unwrap();
    assert!(
        fetch_one(&consumer).await.is_none(),
        "the duplicate was never stored"
    );

    t.cleanup().await;
}

#[tokio::test]
async fn poison_is_copied_to_the_dlq_and_max_deliver_writes_the_advisory() {
    let t = test_nats().await;
    let root = t.subject("retry");
    let stream = t.stream(&root).await;
    let dlq_name = format!("{}_DLQ", t.stream_name());
    // Both dead-letter paths: the worker's own copies and the server's advisory.
    let dlq = t
        .jetstream
        .get_or_create_stream(stream::Config {
            name: dlq_name.clone(),
            subjects: vec![
                format!("dlq.{}.>", t.prefix),
                format!(
                    "$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.{}.>",
                    t.stream_name()
                ),
            ],
            storage: stream::StorageType::Memory,
            ..Default::default()
        })
        .await
        .unwrap();
    for name in ["poison", "fixable"] {
        t.jetstream
            .publish(format!("{root}.{name}"), name.into())
            .await
            .unwrap()
            .await
            .unwrap();
    }
    let consumer = stream
        .get_or_create_consumer("retrier", pull_config("retrier", format!("{root}.>")))
        .await
        .unwrap();

    // First delivery of both. The poison one is dead-lettered, the other nak'd.
    let first = fetch_one(&consumer).await.unwrap();
    assert_eq!(first.payload, "poison");
    assert_eq!(first.info().unwrap().delivered, 1);
    dead_letter(&first, "undecodable").await.unwrap();
    // The copy is durable before the Term, so it is already there.
    let copy = dlq.get_raw_message(1).await.unwrap();
    assert_eq!(copy.subject.as_str(), format!("dlq.{root}.poison"));
    assert_eq!(copy.payload, "poison");
    assert_eq!(copy.headers.get("Dlq-Reason").unwrap().as_str(), "undecodable");
    let second = fetch_one(&consumer).await.unwrap();
    assert_eq!(second.payload, "fixable");
    second.ack_with(AckKind::Nak(None)).await.unwrap();

    // A nak redelivers at once; this is delivery 2 of max_deliver 2.
    let again = fetch_one(&consumer).await.unwrap();
    assert_eq!(again.payload, "fixable");
    assert_eq!(again.info().unwrap().delivered, 2);
    again.ack_with(AckKind::Nak(None)).await.unwrap();

    // The advisory fires on the next delivery attempt, not on the nak: this
    // empty fetch is what makes the server evaluate max_deliver.
    assert!(
        fetch_one(&consumer).await.is_none(),
        "terminated and exhausted"
    );

    let advisory = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(raw) = dlq.get_raw_message(2).await {
                return raw.payload;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("advisory reaches the dlq stream");
    let body: serde_json::Value = serde_json::from_slice(&advisory).unwrap();
    assert_eq!(body["type"], "io.nats.jetstream.advisory.v1.max_deliver");
    assert_eq!(body["consumer"], "retrier");
    assert_eq!(body["deliveries"], 2);

    t.jetstream.delete_stream(&dlq_name).await.unwrap();
    t.cleanup().await;
}

/// The trap: `messages()` yields one `Err` item when the consumer goes away
/// and then stays pending (only `ConsumerDeleted` also ends it). A `?` there
/// retires the worker for good; a `continue` hangs it.
#[tokio::test]
async fn deleting_the_consumer_errors_the_messages_stream() {
    let t = test_nats().await;
    let root = t.subject("gone");
    let stream = t.stream(&root).await;
    let consumer = stream
        .get_or_create_consumer("doomed", pull_config("doomed", format!("{root}.>")))
        .await
        .unwrap();
    let mut messages = consumer.messages().await.unwrap();

    stream.delete_consumer("doomed").await.unwrap();

    let err = tokio::time::timeout(Duration::from_secs(10), messages.next())
        .await
        .expect("the stream reacts")
        .expect("an item")
        .expect_err("the consumer is gone");
    // Which kind depends on timing: usually `no responders` to the next pull;
    // `consumer deleted` when the server answers a pull in flight with the
    // `409 Consumer Deleted` status first.
    assert!(matches!(
        err.kind(),
        MessagesErrorKind::ConsumerDeleted | MessagesErrorKind::NoResponders
    ));

    t.cleanup().await;
}

#[tokio::test]
async fn request_reply_no_responders_and_timeout() {
    let t = test_nats().await;
    let subject = t.subject("pricing.quote");

    let err = t
        .client
        .request(subject.clone(), "sku".into())
        .await
        .expect_err("nothing is subscribed");
    assert_eq!(
        err.kind(),
        RequestErrorKind::NoResponders,
        "immediate, not after the timeout"
    );

    let responder = t.client.clone();
    let responder_subject = subject.clone();
    let task = tokio::spawn(async move {
        let mut requests = responder
            .queue_subscribe(responder_subject, "pricing".to_owned())
            .await
            .unwrap();
        while let Some(message) = requests.next().await {
            // No `respond`: publish to the reply subject.
            let reply = message.reply.expect("requests carry a reply subject");
            responder.publish(reply, "4200".into()).await.unwrap();
        }
    });
    // Subscriptions register asynchronously; flush before requesting.
    t.client.flush().await.unwrap();

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        t.client.request(subject, "sku".into()),
    )
    .await
    .expect("in time")
    .expect("round trip");
    assert_eq!(response.payload, "4200");

    // A subscriber that never answers: the per-request timeout fires.
    let slow = t.subject("pricing.slow");
    let _held = t.client.subscribe(slow.clone()).await.unwrap();
    t.client.flush().await.unwrap();
    let err = t
        .client
        .send_request(
            slow,
            async_nats::Request::new()
                .payload("sku".into())
                .timeout(Some(Duration::from_millis(200))),
        )
        .await
        .expect_err("nobody answers");
    assert_eq!(err.kind(), RequestErrorKind::TimedOut);
    task.abort();
}

#[tokio::test]
async fn queue_group_delivers_each_message_once() {
    let t = test_nats().await;
    let root = t.subject("events");
    let mut one = t
        .client
        .queue_subscribe(format!("{root}.>"), "workers".to_owned())
        .await
        .unwrap();
    let mut two = t
        .client
        .queue_subscribe(format!("{root}.>"), "workers".to_owned())
        .await
        .unwrap();
    t.client.flush().await.unwrap();

    for index in 0..20 {
        t.client
            .publish(format!("{root}.eu.created"), format!("{index}").into())
            .await
            .unwrap();
    }
    t.client.flush().await.unwrap();

    let mut seen = 0;
    for _ in 0..20 {
        let message = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                Some(message) = one.next() => message,
                Some(message) = two.next() => message,
            }
        })
        .await
        .expect("a member receives it");
        assert_eq!(message.subject.as_str(), format!("{root}.eu.created"));
        seen += 1;
    }
    assert_eq!(seen, 20);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), one.next())
            .await
            .is_err(),
        "nothing is delivered twice"
    );
}

#[tokio::test]
async fn kv_put_get_update_and_watch() {
    let t = test_nats().await;
    let bucket = format!("b{}", t.prefix);
    let store = t
        .jetstream
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: bucket.clone(),
            history: 5,
            ..Default::default()
        })
        .await
        .unwrap();

    let revision = store.put("flag", "on".into()).await.unwrap();
    assert_eq!(
        store.get("flag").await.unwrap().as_deref(),
        Some(&b"on"[..])
    );

    // `watch_with_history` delivers the current value first; plain `watch`
    // would deliver nothing until the update below.
    let mut watch = store.watch_with_history("flag").await.unwrap();
    let current = tokio::time::timeout(Duration::from_secs(5), watch.next())
        .await
        .expect("the current value")
        .expect("an entry")
        .unwrap();
    assert_eq!(current.value, "on");
    assert_eq!(current.revision, revision);

    store.update("flag", "off".into(), revision).await.unwrap();
    let entry = tokio::time::timeout(Duration::from_secs(5), watch.next())
        .await
        .expect("a change")
        .expect("an entry")
        .unwrap();
    assert_eq!(entry.value, "off");
    assert_eq!(entry.revision, revision + 1);
    // Compare-and-swap with a stale revision fails.
    assert!(
        store
            .update("flag", "stale".into(), revision)
            .await
            .is_err()
    );

    t.jetstream.delete_key_value(&bucket).await.unwrap();
}
```

## What is worth a test

- the handler: a struct literal in, an `Outcome` out — no broker;
- a duplicate `Nats-Msg-Id` returns the original sequence and is never delivered;
- a nak redelivers with `delivered` incremented, a poison message's copy lands on
  `dlq.<subject>` before the term, and `max_deliver` puts the advisory in the dead-letter stream;
- `messages()` yields an error item when the consumer is deleted, and the worker survives it;
- `no responders` is immediate and `TimedOut` respects the per-request timeout;
- a queue group hands each message to exactly one member;
- `traceparent` injected into the headers comes back out with the same trace id;
- a KV watcher sees the current value before the first change.

## Compose and CI

The compose `nats` service (`nats:2.12-alpine` with `-js`, healthcheck on `:8222/healthz`) and the
CI `services:` entry live in `rust-tooling`'s optional-services block, next to Redis. The contract
this side needs is only that both export `TEST_NATS_URL`, so no test talks to Docker there; the
container above is the fallback for a machine without either.
