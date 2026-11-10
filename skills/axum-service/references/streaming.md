# SSE and streaming responses

## Contents

- [Server-sent events](#server-sent-events)
- [Streaming a body](#streaming-a-body)

A streaming response needs a `Stream`. `tokio-stream` is in the base dependency set for exactly
this: it is the tokio-native way to turn a channel or an interval into a stream, so nothing is
added to `Cargo.toml`.

## Server-sent events

```rust,verify
//! `api/events.rs` — an SSE endpoint fed by a worker task.
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use sea_orm::ConnectionTrait as _;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt as _};

use crate::AppState;
use crate::error::AppError;

pub async fn events(
    State(state): State<AppState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    // Everything that can fail happens before the first event: once the stream
    // starts, the response head is gone and an error cannot become a 500.
    state.db.ping().await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<String>(16);

    tokio::spawn(async move {
        for step in 0..10_u32 {
            // `send` fails once the client disconnects and the receiver drops,
            // which is how the producer learns to stop.
            if tx.send(format!("step {step}")).await.is_err() {
                tracing::debug!("client disconnected");
                return;
            }
        }
    });

    let stream =
        ReceiverStream::new(rx).map(|line| Ok(Event::default().event("progress").data(line)));

    // A keep-alive comment every 15 s stops proxies from closing an idle stream.
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// Merge this router like any other, *inside* the `.layer(..)` stack in
/// `build_router`, so the stream keeps the request id, panic handling, tracing,
/// metrics and CORS. `api::request_timeout` races only the future that produces
/// the response, so it never cuts a body that has already started.
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/events", axum::routing::get(events))
}
```

Things that bite:

- **The request timeout does not cut the stream.** `api::request_timeout` races the future that
  produces the response; once the head is out the body is passed through untouched, so a 30-second
  budget never truncates an SSE stream. Mounting the route outside `.layer(..)` to "protect" it drops it
  out of every other layer too — request id, panic handling, tracing, metrics, CORS — so merge it
  inside the stack like any other router.
- **What ends a long stream is outside the process.** Reverse-proxy and load-balancer idle
  timeouts close a connection that has been quiet; that is what the keep-alive above is for. To
  bound a stream from inside, wrap the stream rather than the handler — `StreamExt::timeout` for
  a per-item deadline, or a `take_while` on an elapsed budget:

  ```rust
  let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
  let stream = stream.take_while(move |_| tokio::time::Instant::now() < deadline);
  ```

  `take_while` is evaluated only when the next item arrives, so it bounds a busy stream and never
  a stalled one — that is what `StreamExt::timeout` is for. It yields one `Elapsed` and then waits
  for the next item again, so end the stream there, as in `stream.timeout(d).map_while(Result::ok)`,
  or the keep-alive holds the stalled connection open.

  tower-http has the layer forms, for a whole subtree instead of one stream, behind its `timeout`
  feature, which the scaffold does not enable. They are two different timers.
  `ResponseBodyTimeoutLayer` is an idle timer that restarts on every frame, so on an SSE route the
  keep-alive comment resets it and it never fires; it catches a stalled stream only where nothing
  is sent while idle. `ResponseBodyDeadlineLayer` starts once and never resets: it caps the total
  time a body may take, however often data arrives.
- **Shutdown does not wait for a stream.** A stream never closes on its own, so `main.rs` stops
  draining `SHUTDOWN_TIMEOUT` after the signal and the connection is cut. A client of a long
  stream has to handle a reconnect anyway; give each event an `id` when it must resume where it
  stopped, and read `Last-Event-ID` on the way back in.
- **The status is already sent.** Once the first event goes out, the response head is gone and an
  error cannot become a 500 — hence the pre-flight check above. Report a mid-stream failure as a
  final `Event::default().event("error")` and end the stream.
- **Compression leaves SSE alone and holds other streams back.** `CompressionLayer`'s default
  predicate skips `text/event-stream`, so events are delivered as they are sent. Any other streamed
  body (NDJSON, a chunked export) is compressed, and the encoder keeps bytes until it has a block
  to emit: lines arrive late and in bursts. For a stream a client reads incrementally, exclude its
  content type, as shown under [Streaming a body](#streaming-a-body).
- **Backpressure is the channel.** A bounded channel makes a slow client slow the producer down,
  which is what you want. Unbounded means one stalled reader grows the heap forever.
- The item type is `Result<Event, E>`; use `Infallible` when the producer cannot fail, so nothing
  in the handler has to invent an error.

## Streaming a body

For a large file or an NDJSON export, build the body from a stream instead of collecting it:

```rust
use axum::body::Body;
use axum::response::Response;

let stream = ReceiverStream::new(rx).map(Ok::<_, std::io::Error>);
Response::builder()
    .header("content-type", "application/x-ndjson")
    .body(Body::from_stream(stream))
```

If a client reads the lines as they arrive, keep the content type out of compression. This
replaces `CompressionLayer::new()` in `build_router`; `DefaultPredicate` keeps the rules the
default layer had (nothing under 32 bytes, no gRPC, no images, no SSE):

```rust,verify
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate as _};

pub fn with_compression(router: axum::Router) -> axum::Router {
    router.layer(CompressionLayer::new().compress_when(
        DefaultPredicate::new().and(NotForContentType::const_new("application/x-ndjson")),
    ))
}
```

The same caveat applies as for SSE: the status and headers go out first, so an error after the
first chunk can only truncate the response. For a download, set `content-disposition` and, when the length is
known in advance, `content-length` — clients show no progress without it.

For streaming LLM output over SSE see `building-rig-agents`, which produces the token stream; the
transport rules above still apply.
