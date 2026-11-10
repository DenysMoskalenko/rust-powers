# A regression test for per-connection memory

Checked against axum 0.8.9, stats_alloc 0.1.10, tokio 1.53 and cargo-nextest 0.9.

- [Why the API tests miss it](#why-the-api-tests-miss-it)
- [The test](#the-test)
- [What each part is for](#what-each-part-is-for)

## Why the API tests miss it

A server that copies something for every accepted connection passes every API test: `TestServer`
opens no socket. This test holds keep-alive connections open on raw `TcpStream`s, since a pooled
client would reuse one. The bug it exists for is a bare `Router` handed to `axum::serve`
(`axum-service` owns that rule).

## The test

Add `stats_alloc = "0.1"` to `[dev-dependencies]`. tikv-jemalloc-sys needs `make`, which slim CI
images lack.

```rust,verify,test
//! `tests/api/connection_memory.rs`, declared with `mod connection_memory;` in `main.rs`.
#![allow(clippy::unwrap_used, reason = "test helpers")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use app::clock::SystemClock;
use app::config::Settings;
use app::{AppState, build_router};
use axum::routing::get;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

// Counts every allocation in the process: one per test binary, and the numbers
// only mean something when nothing else runs in that process.
#[global_allocator]
static ALLOCATOR: &StatsAlloc<std::alloc::System> = &INSTRUMENTED_SYSTEM;

const CONNECTIONS: usize = 50;
const BALLAST_BYTES: usize = 1024 * 1024;
/// A server that copies the router holds `CONNECTIONS * BALLAST_BYTES`, 50 MiB.
const BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// The router needs a state, not a database: the pool connects on first use,
/// and `/health/live` never uses it.
async fn state() -> AppState {
    let url = "postgres://postgres:postgres@127.0.0.1:1/unused";
    let settings = Settings::from_map(HashMap::from([
        ("APP__DATABASE__URL".to_owned(), url.to_owned()),
        ("APP__AUTH__JWT_SECRET".to_owned(), "test-secret".to_owned()),
    ]))
    .unwrap();
    let mut options = sea_orm::ConnectOptions::new(url);
    options.connect_lazy(true);
    AppState {
        db: sea_orm::Database::connect(options).await.unwrap(),
        settings: Arc::new(settings),
        clock: Arc::new(SystemClock),
        http: reqwest_middleware::ClientBuilder::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
        )
        .build(),
    }
}

/// One request on a raw socket, which is then handed back still open.
async fn keep_alive(address: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream
        .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    // `read` may return half a status line, which is a flake; `read_exact` waits.
    let mut status = [0_u8; 12];
    stream.read_exact(&mut status).await.unwrap();
    assert_eq!(&status, b"HTTP/1.1 200");
    stream
}

#[tokio::test]
async fn open_connections_do_not_copy_the_router() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    // A route that owns a megabyte, so a copy of the router is impossible to miss
    // however small the real routes are.
    let ballast = vec![0_u8; BALLAST_BYTES];
    let app = build_router(state().await).route(
        "/ballast",
        get(move || {
            let size = ballast.len();
            async move { size.to_string() }
        }),
    );
    // The same call as `main.rs`. If the project has a `serve` function in its
    // library, call that instead: a copied line cannot notice `main.rs` changing.
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });

    // The first request pays for every lazy static; keep it out of the region.
    let _warm_up = keep_alive(address).await;

    let region = Region::new(ALLOCATOR);
    let mut open = Vec::with_capacity(CONNECTIONS);
    for _ in 0..CONNECTIONS {
        open.push(keep_alive(address).await);
    }
    let change = region.change();

    let grown = change
        .bytes_allocated
        .saturating_sub(change.bytes_deallocated);
    assert!(
        grown < BUDGET_BYTES,
        "{CONNECTIONS} open connections hold {grown} bytes"
    );
}
```

## What each part is for

- **Net growth** is `bytes_allocated - bytes_deallocated`. Do not add `bytes_reallocated`:
  `realloc` already feeds both counters, so that counts every grown buffer twice.
- **The ballast route decides the result.** On the scaffold, 50 connections hold 1.4 MB served
  through the make-service and only 2.9 MB bare, because its routes capture little. With the
  megabyte route the bare form holds 55.3 MB.
- **nextest only.** The counters are process-wide, and nextest runs one test per process. Under
  `cargo test`, give this test a binary of its own.
- **One `#[global_allocator]` per crate graph.** If the library crate declares one, move it to
  `main.rs`.
