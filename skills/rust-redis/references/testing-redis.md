# Testing against a real Redis

- [Why the real server](#why-the-real-server)
- [The nextest process model](#the-nextest-process-model)
- [Isolation: a key prefix](#isolation-a-key-prefix)
- [The fixture and the tests](#the-fixture-and-the-tests)
- [What is worth a test](#what-is-worth-a-test)
- [Compose and CI](#compose-and-ci)

## Why the real server

Redis is infrastructure, so the one-implementor-trait exception `rust-code-style` allows does not
buy a fake here: the behaviour under test is TTL expiry, `SET NX` losing a race, `WRONGTYPE`, Lua atomicity
and head-of-line blocking, and none of it exists in a substitute.

`redis-test`'s `MockRedisConnection` replays a fixed, ordered list of command-and-reply pairs and
asserts byte-for-byte command equality, so a jittered TTL argument or an added `EXPIRE` breaks the
test for no reason, and it models neither expiry nor server errors. Its one legitimate use is
unit-testing a custom `ConnectionLike` wrapper, or forcing an error branch a healthy server will
not produce on demand.

## The nextest process model

cargo-nextest runs every test in its own process, so a `static` or `OnceCell` holding a container
initialises once per *test*, not once per binary - one container per test, and a pile of them left
running. Two arrangements survive that:

1. **An external server.** `TEST_REDIS_URL` points at a compose service locally and at a CI
   service container in the pipeline. Nothing to start, nothing to clean up, fastest in CI.
2. **A reusable container.** A fixed name plus `ReuseDirective::Always`: the first process to
   start it wins and every later process attaches to the same one.

The reusable container has a race the Postgres equivalent shares. N test processes start at the
same instant, all of them try to create the one named container, and every loser gets
`409 Conflict: container name is already in use` - measured at 11 failures out of 20 tests on a
cold run. Retrying `start()` fixes it, because by the retry the winner's container exists and the
call attaches to it instead.

The module's default image tag is `5.0`, a 2018 server with no `SET ... KEEPTTL`. Always call
`.with_tag(..)` with the version production runs.

## Isolation: a key prefix

Give every test a `test:{uuid}` prefix and route every key through it. It works against a shared
container, a compose service and a CI service alike, needs no cleanup step, and survives full
parallelism.

The alternatives do not. `FLUSHDB` on a dedicated database index is correct only with
`test-threads = 1` or one index per test, and Redis has 16 by default - with tests running
concurrently in separate processes, one test's `FLUSHDB` wipes another's data. A container per
test is correct and only costs about 200 ms to start, but it multiplies the 409 race below by the
number of tests and leaves N containers to clean up; one shared container is the simpler shape.

## The fixture and the tests

The container comes from the testcontainers module crate. The dev-dependency already carries
the feature — the stack pins all three modules together, so adding Redis tests changes nothing
in `Cargo.toml`:

```toml
[dev-dependencies]
testcontainers-modules = { version = "0.15", features = ["postgres", "redis", "nats"] }
testcontainers = { version = "0.27", features = ["reusable-containers"] }   # modules does not re-export it
```

In a real suite the fixture lives in `tests/common/mod.rs` and each test binary opens with
`mod common;`. It is shown inline here so the file is complete.

```rust,verify,test
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "allow-unwrap-in-tests covers test bodies, not helpers"
)]

use std::time::{Duration, Instant};

use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions, aio::ConnectionManager};
use testcontainers::{ContainerAsync, ImageExt, ReuseDirective, runners::AsyncRunner};
use testcontainers_modules::redis::{REDIS_PORT, Redis};
use uuid::Uuid;

pub struct TestRedis {
    pub conn: ConnectionManager,
    pub url: String,
    /// Every key this test writes starts with this, so tests sharing one server
    /// cannot see each other. Cheaper and safer than `FLUSHDB`.
    pub prefix: String,
    _container: Option<ContainerAsync<Redis>>,
}

impl TestRedis {
    #[must_use]
    pub fn key(&self, name: &str) -> String {
        format!("{}:{name}", self.prefix)
    }
}

pub async fn test_redis() -> TestRedis {
    let prefix = format!("test:{}", Uuid::now_v7());
    if let Ok(url) = std::env::var("TEST_REDIS_URL") {
        let conn = connect(&url).await;
        return TestRedis {
            conn,
            url,
            prefix,
            _container: None,
        };
    }
    let container = start_reusable().await;
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(REDIS_PORT)
        .await
        .expect("container port");
    let url = format!("redis://{host}:{port}");
    let conn = connect(&url).await;
    TestRedis {
        conn,
        url,
        prefix,
        _container: Some(container),
    }
}

/// Every nextest process races to create the one named container and all but
/// the winner get a Docker 409. The retry finds the winner's container.
async fn start_reusable() -> ContainerAsync<Redis> {
    let mut last = String::new();
    for attempt in 0..30_u32 {
        let image = Redis::default()
            // The module default is 5.0, from 2018. Pin what production runs.
            .with_tag("8-alpine")
            .with_container_name("rust-powers-test-redis")
            .with_reuse(ReuseDirective::Always);
        match image.start().await {
            Ok(container) => return container,
            Err(e) => {
                last = e.to_string();
                tokio::time::sleep(Duration::from_millis(200 + u64::from(attempt) * 100)).await;
            }
        }
    }
    panic!("could not start or reuse the redis container: {last}")
}

/// A reused container is only "started" for the process that created it; the
/// others attach to one that may still be booting.
async fn connect(url: &str) -> ConnectionManager {
    let client = redis::Client::open(url).expect("redis url");
    // Generous on purpose: a cold container is slower than production.
    let config = redis::aio::ConnectionManagerConfig::new()
        .set_connection_timeout(Some(Duration::from_secs(5)))
        .set_response_timeout(Some(Duration::from_secs(2)));
    for attempt in 0..20_u32 {
        if let Ok(mut conn) = client
            .get_connection_manager_with_config(config.clone())
            .await
            && redis::cmd("PING").exec_async(&mut conn).await.is_ok()
        {
            return conn;
        }
        tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt + 1))).await;
    }
    panic!("redis never became ready at {url}")
}

#[tokio::test]
async fn a_value_expires_and_then_reads_as_none() {
    let t = test_redis().await;
    let mut conn = t.conn.clone();
    let key = t.key("short");

    conn.set_ex::<_, _, ()>(&key, "v", 1).await.unwrap();
    let ttl: i64 = conn.ttl(&key).await.unwrap();
    assert_eq!(ttl, 1, "every write carries a TTL");

    // Real time, not `tokio::time::pause`: the expiry happens in the server.
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    let gone: Option<String> = conn.get(&key).await.unwrap();
    assert_eq!(gone, None);
}

#[tokio::test]
async fn a_held_lease_is_not_handed_out_twice_and_release_compares_the_token() {
    let t = test_redis().await;
    let mut conn = t.conn.clone();
    let key = t.key("lock:reindex");
    let take = |token: &'static str, ms: u64| {
        let mut conn = t.conn.clone();
        let key = key.clone();
        async move {
            let options = SetOptions::default()
                .conditional_set(ExistenceCheck::NX)
                .with_expiration(SetExpiry::PX(ms));
            conn.set_options::<_, _, Option<String>>(&key, token, options)
                .await
                .unwrap()
        }
    };

    assert!(take("mine", 200).await.is_some(), "uncontended acquire");
    assert!(take("theirs", 200).await.is_none(), "a held lease is not free");

    tokio::time::sleep(Duration::from_millis(300)).await; // the lease expires
    assert!(take("theirs", 10_000).await.is_some(), "expired lease is free");

    // The previous holder tries to release. A plain DEL would free somebody
    // else's lock; the compare-and-delete refuses.
    let release = redis::Script::new(
        r"if redis.call('GET', KEYS[1]) == ARGV[1] then
              return redis.call('DEL', KEYS[1])
          else
              return 0
          end",
    );
    let deleted: i64 = release
        .key(&key)
        .arg("mine")
        .invoke_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(deleted, 0, "compare-and-delete must refuse");
    let still_theirs: Option<String> = conn.get(&key).await.unwrap();
    assert_eq!(still_theirs.as_deref(), Some("theirs"));
}

#[tokio::test]
async fn wrong_type_is_a_bug_not_an_outage() {
    let t = test_redis().await;
    let mut conn = t.conn.clone();
    let key = t.key("wrongtype");
    conn.sadd::<_, _, ()>(&key, "member").await.unwrap();

    let err = conn
        .get::<_, Option<String>>(&key)
        .await
        .expect_err("GET on a set must fail");

    assert_eq!(err.code(), Some("WRONGTYPE"));
    // The classification a cache layer branches on: this one must not degrade
    // to a miss, because retrying it will fail the same way forever.
    assert!(!err.is_timeout() && !err.is_connection_dropped() && !err.is_io_error());
}

#[tokio::test]
async fn a_blocking_command_stalls_every_other_caller_on_one_connection() {
    let t = test_redis().await;
    let list = t.key("queue");
    let probe = t.key("probe");
    let mut setup = t.conn.clone();
    setup.set_ex::<_, _, ()>(&probe, "v", 60).await.unwrap();

    let mut blocker = t.conn.clone();
    let blocking = tokio::spawn(async move {
        let _: Option<(String, String)> = redis::cmd("BLPOP")
            .arg(&list)
            .arg(2)
            .query_async(&mut blocker)
            .await
            .unwrap_or(None);
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A clone of the same multiplexed connection, so this GET queues behind the
    // BLPOP: measured at 1.94 s. With a 100 ms response timeout it would fail.
    let mut other = t.conn.clone();
    let started = Instant::now();
    let got: Result<Option<String>, _> = other.get(&probe).await;
    let waited = started.elapsed();
    blocking.await.unwrap();

    assert!(
        waited > Duration::from_millis(800) || got.is_err(),
        "expected the GET to queue behind BLPOP, got {got:?} after {waited:?}"
    );
}
```

## What is worth a test

Test the behaviour the helpers encode, not the helpers' plumbing:

- a cached value comes back, a second call does not call the loader, and the key carries a TTL;
- a missing row is cached as `null`, so a 404 does not re-query;
- invalidation forces the next read to reload;
- a lease is not handed out twice, and a stale holder cannot release the new one;
- the limiter allows exactly `limit` calls and then refuses, and the next window allows again;
- an idempotency key claims once and replays afterwards;
- `WRONGTYPE` is classified as a bug, and an unreachable server as an outage.

The last one needs no container: point the manager at `redis://127.0.0.1:1`, which is closed
everywhere, and assert `is_connection_refusal()`. Eager connection makes that fail at construction
time. Use the production config with `set_number_of_retries(0)` - the manager's default of six
retries with backoff takes 9.5 s to report the refusal, the zero-retry config under a millisecond.

Expiry tests wait on the clock, because the expiry happens inside the server and
`tokio::time::pause` cannot move it. Keep those TTLs at a second or two and there is nothing to
optimise.

## Compose and CI

The compose `redis` service (`redis:8-alpine`) and the CI `services:` entry that exports
`TEST_REDIS_URL` live in `rust-tooling`'s optional-services block, next to NATS; the fixture
above is the fallback for a machine without either. Two flags on that service matter here:
`--save "" --appendonly no` turns off persistence, which a cache does not want and which is the
main source of surprise latency in a test run, and `--maxmemory-policy noeviction` (the server
default, spelled out) is what a Redis holding leases, idempotency markers or limiter buckets
needs in production too: under memory pressure `allkeys-lru` - and every `volatile-*` policy,
since all those keys carry TTLs - silently drops a lease (two holders), a marker (a duplicate
side effect) or a bucket (a reset limit). `allkeys-lru` is right only for an instance that does
nothing but cache; a service that does both either runs two instances or accepts `OOM` errors as
the loud failure they are.

Test organisation, fixtures and factories belong to `rust-testing`; what is above is only the
Redis half.
