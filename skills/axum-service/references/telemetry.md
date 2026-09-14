# Tracing, OTLP export and propagation

Assumes tracing 0.1 with tracing-subscriber 0.3, tracing-opentelemetry 0.33, opentelemetry 0.32
(`opentelemetry_sdk`, `opentelemetry-otlp`), axum-tracing-opentelemetry 0.39, and
reqwest-middleware 0.5 with reqwest-tracing 0.7.

## Contents

[The four wires](#the-four-wires) · [tracing conventions](#tracing-conventions) ·
[Subscriber](#subscriber-filter-and-format) · [telemetry.rs](#telemetryrs) ·
[main ordering](#main-ordering-and-shutdown) · [Request id](#request-id) ·
[HTTP spans](#http-spans-otelaxumlayer-and-otelinresponselayer) · [Outbound](#outbound-propagation) ·
[Environment](#environment-variables) · [Debugging](#debugging) ·
[Tests](#verifying-telemetry-in-tests) · [Red flags](#red-flags) ·
[Older APIs](#older-opentelemetry-apis)

## The four wires

Observability is four independent wires, and three of them fail without a sound:

| Wire | Crate | How it fails |
|---|---|---|
| Logs | tracing-subscriber | loudly — nothing on stdout |
| Traces | opentelemetry-otlp | silently — no spans in the backend |
| Propagation | `TraceContextPropagator` | silently — every service starts its own trace |
| Metrics | metrics, axum-prometheus | silently — `/metrics` renders an empty body |

The first three are wired in `src/telemetry.rs::init` and nowhere else: a second
`tracing_subscriber::…init()` anywhere in the process makes one of them lose, and a second
`global::set_tracer_provider` drops the first provider's spans on the floor. Metrics are wired once
in `build_router`, where `PrometheusMetricLayer::pair()` installs the recorder.

## tracing conventions

Instrument the function that does the work, not every layer it passes through. `OtelAxumLayer`
already opens a span per request, so a handler that extracts and delegates adds nothing but noise.

```rust
#[instrument(skip_all, fields(user_id = %id), err)]
pub async fn deactivate(db: &DatabaseConnection, id: Uuid, reason: &SecretString)
    -> Result<(), AppError>
```

- **`skip_all` plus explicit `fields(..)`**, not `skip(db)`: with `skip`, an argument added later
  silently becomes a field. Connections, pools, clients, bodies and credentials stay out.
- **`%` is `Display`, `?` is `Debug`.** `%` on a `SecretString` prints the secret — `secrecy` redacts
  `Debug` only. Never `%` a token, password or `Authorization` header, and never field a whole body.
- **`err` emits only on `Err`**, at ERROR, formatted with `Display`. `err(Debug)` gives the whole
  chain; `err(level = Level::INFO)` lowers an expected failure such as a 404. It belongs on a
  service function like `deactivate` above, never on a handler returning `AppError`:
  `into_response` already logs every 5xx once, so `err` on the handler double-logs those and
  records each 404 and 422 as an error event.
- **The field name `error` is load-bearing.** tracing-opentelemetry turns an event into an OTel
  `exception` only when it carries a field named `error` *and* has an empty message, so
  `#[instrument(err)]` and `tracing::error!(error = %e);` both produce one while
  `tracing::error!(error = ?e, "upstream failed")` stays a plain event with an `error` attribute.
- **Log an error once**, where request context still exists — `AppError::into_response`. One failure
  logged at three depths reads as three incidents.
- **Span names are low cardinality**, like metric labels: the function name or a fixed string, never
  an interpolated id. Attribute names follow OTel semantic conventions (`http.request.method`,
  `http.route`, `url.path`, `server.address`), plus the four magic fields `otel.name`, `otel.kind`,
  `otel.status_code` and `otel.status_description`, which set the span's OTel identity rather than
  adding an attribute.

```rust,ignore
// Every argument becomes a field, so the token is now in the log line and in the backend.
#[instrument]
pub async fn refresh(pool: &DatabaseConnection, token: String) -> Result<Session, AppError>
```

| Level | Means | Example |
|---|---|---|
| ERROR | a request was lost or state is inconsistent; someone should look | 5xx, failed write after retries |
| WARN | degraded but handled | retry, fallback, upstream 429 |
| INFO | request boundaries and lifecycle | startup, migration applied, shutdown |
| DEBUG | enough to reconstruct one request locally | branch taken, query parameters |
| TRACE | never enabled in production | per-row loops |

A 4xx is not an ERROR: the client made a mistake and the service did its job. Logging expected
validation failures at ERROR trains everyone to ignore the level.

**PII:** log the id, not the person. `user_id`, `tenant_id`, `order_id`, `job_id`, the upstream name
and an idempotency key are safe fields; email addresses, names, postal addresses, card fragments and
free-text input are not, and redaction after the fact is a data-deletion project.

## Subscriber: filter and format

`EnvFilter::try_from_default_env()` reads `RUST_LOG`, with `info` as the fallback so a bare
`cargo run` still logs. The filter sits on the whole registry, so `RUST_LOG=warn` also removes the
`info` spans the OTel layer would have exported — the usual reason traces disappear right after
someone quiets the logs. One directive is always appended: `otel::tracing=trace`, because
`OtelAxumLayer` emits its HTTP span at TRACE under that target, and without it the span is
filtered out and every request logs a warning.

Format is a setting (`json_logs`), not `cfg!(debug_assertions)`: JSON in every deployed environment,
pretty locally, switchable without a rebuild. The two `fmt::layer()` variants are different types,
so both are `.boxed()` to share one variable. `.flatten_event(true)` on the JSON layer puts the
fields at the top level where a log pipeline can index them; add it when the pipeline needs it.

## telemetry.rs

```rust,verify
//! `src/telemetry.rs` — logs and traces. One call in `main`, one guard to drop at the end.
use anyhow::Context as _;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

use crate::config::TelemetrySettings;

/// Holds the tracer provider so spans can be flushed before the process exits.
pub struct TelemetryGuard {
    provider: SdkTracerProvider,
}

impl TelemetryGuard {
    /// Flushes buffered spans. Skipping this loses every span still in the batch
    /// queue, which is most of them for a short-lived process.
    pub fn shutdown(self) {
        if let Err(error) = self.provider.shutdown() {
            tracing::warn!(%error, "tracer provider shutdown failed");
        }
    }
}

/// Installs the subscriber. Call once, before anything logs.
///
/// # Errors
/// The OTLP exporter cannot be built (bad endpoint, no tokio runtime).
pub fn init(settings: &TelemetrySettings) -> anyhow::Result<TelemetryGuard> {
    // `RUST_LOG` wins; "info" is the fallback so a bare `cargo run` still logs.
    // `OtelAxumLayer` emits its HTTP span at TRACE under `otel::tracing`; without
    // that directive the span is filtered out and every request logs a warning.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("otel::tracing=trace".parse()?);

    // Two different layer types, so both need boxing to share one variable.
    let fmt_layer = if settings.json_logs {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().pretty().boxed()
    };

    let provider = build_tracer_provider(settings.otlp_endpoint.as_deref())?;

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        // Always on, exporter or not: `OtelAxumLayer` needs a real span context
        // to attach `traceparent` to, and warns on every request without one.
        // Without `use opentelemetry::trace::TracerProvider as _` this method
        // does not exist.
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer(env!("CARGO_PKG_NAME"))))
        .init();

    Ok(TelemetryGuard { provider })
}

/// The provider always exists so trace ids are minted and propagated; only the
/// exporter is optional. `None` means spans are created and dropped locally.
fn build_tracer_provider(endpoint: Option<&str>) -> anyhow::Result<SdkTracerProvider> {
    let mut builder = SdkTracerProvider::builder()
        // Picks up OTEL_SERVICE_NAME and the other OTEL_RESOURCE_ATTRIBUTES.
        .with_resource(Resource::builder().build());

    if let Some(endpoint) = endpoint {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .build()
            .context("building the OTLP span exporter")?;
        builder = builder.with_batch_exporter(exporter);
    }

    let provider = builder.build();
    global::set_tracer_provider(provider.clone());
    // The default global propagator is a no-op: without this line no trace
    // context is read or written and nothing warns about it.
    global::set_text_map_propagator(TraceContextPropagator::new());
    Ok(provider)
}
```

`Settings` carries this as `telemetry: TelemetrySettings` — `otlp_endpoint: Option<String>` (`None`
builds no exporter, which is what local runs and tests want; the provider and the propagator are
installed either way so trace ids exist and `traceparent` still flows) and `json_logs: bool`. The
setting is the only switch: with `None` the `SpanExporter` builder — the one thing that would read
`OTEL_EXPORTER_OTLP_ENDPOINT` — never runs, and with `Some` the value goes to `with_endpoint`, which
overrides that variable. Set `APP__TELEMETRY__OTLP_ENDPOINT`; the SDK variable is inert here. The
exporter is built here, inside the tokio runtime that `main` already runs on: the tonic exporter
needs a reactor, so a `LazyLock` or `OnceLock` provider never exports. Three imports exist only to
bring a trait method into scope, and each has a misleading error:

| Missing import | Error |
|---|---|
| `opentelemetry::trace::TracerProvider as _` | no method named `tracer` |
| `opentelemetry_otlp::WithExportConfig as _` | no method named `with_endpoint` |
| `tracing_subscriber::util::SubscriberInitExt as _` | no method named `init` |

Feature flags matter more than usual. `opentelemetry_sdk` needs `trace` and **not** `rt-tokio`
(since 0.32 the batch processor runs on its own thread; `rt-tokio` only enables the experimental
async-runtime processors), and `opentelemetry-otlp` with `default-features = false` must list
**both** `grpc-tonic` and `trace` — `grpc-tonic` does not imply it, and the defaults drag in a
blocking reqwest.

Do not call `.with_sampler(..)` or `.with_service_name(..)`: the default sampler is
`ParentBased(AlwaysOn)` and the builder reads `OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG`
itself, while `Resource::builder()` runs the environment detectors. Hardcoding either takes the knob
away from whoever operates the service.

## main ordering and shutdown

The one `main.rs` is the scaffold's. The telemetry-relevant order in it:

1. `telemetry::init(&settings.telemetry)` first, on the multi-thread runtime `#[tokio::main]`
   provides, so everything below is logged and traced and the exporter has a reactor.
2. `build_router(state)` installs the HTTP layers and calls `PrometheusMetricLayer::pair()` once.
3. `axum::serve(..).with_graceful_shutdown(..)` drains in-flight requests, which are still
   producing spans.
4. `guard.shutdown()` only now, then `db.close()`.

`guard.shutdown()` blocks until the batch processor has flushed, so it needs the multi-thread
runtime; on a current-thread runtime it deadlocks. Skipping it loses up to
`OTEL_BSP_SCHEDULE_DELAY` (5 s by default) of spans, reliably including the ones that explain why
the process is going down.

## Request id

One `from_fn` middleware, `api::request_id`, owns the id: it trusts an
inbound `x-request-id` or mints a uuid v7, opens `info_span!("request", request_id = %id)` and
`.instrument`s the rest of the request with it, scopes the `REQUEST_ID` task-local declared in
`error.rs` so `ErrorBody::new` can copy the id into the body, and echoes the header. It is the
outermost layer, so every event inside the request, including the ones the Otel layer and the
handlers emit, is logged with the `request` span in its span list. tower-http's `SetRequestIdLayer`
does the header half only and puts the value in neither a span nor reach of the error body, hence
the `from_fn`.

## HTTP spans: OtelAxumLayer and OtelInResponseLayer

One HTTP span source. `OtelAxumLayer` extracts the inbound `traceparent`, takes `http.route` from
`MatchedPath` (so a uuid path is one span name rather than thousands) and emits the HTTP
semantic-convention attributes; `OtelInResponseLayer` sits just outside it and writes the context
back onto the response. Nothing else opens an HTTP span: a second tracing layer (tower-http's
`trace` feature, which the scaffold does not enable) next to `OtelAxumLayer` opens a second,
unrelated server span per request, doubling every log line's context and giving the exporter two
candidate parents for the same work.

Routes registered after these layers are neither traced nor counted. The scaffold registers
everything, `/metrics` and the probes included, before the stack, so a 404 and a scrape carry a
request id; if scrape and probe noise flattens the latency histogram, drop those paths with
`with_ignore_patterns` on the Prometheus layer rather than moving the routes outside the stack.

## Outbound propagation

`global::set_text_map_propagator(TraceContextPropagator::new())` in `init` is what makes propagation
work in both directions. The default global propagator is a no-op, so without it the inbound layer
extracts nothing and the outbound middleware injects nothing — with no error, no warning and no log
line.

```rust,verify
//! The client comes from `AppState`; one built ad hoc sends no `traceparent`.
use reqwest_middleware::ClientWithMiddleware;
use reqwest_tracing::OtelName;

pub async fn fetch_invoice(
    http: &ClientWithMiddleware,
    id: uuid::Uuid,
) -> reqwest_middleware::Result<String> {
    http.get(format!("https://billing.internal/invoices/{id}"))
        // Without this the client span is named after the full URL, so every
        // invoice id becomes its own span name in the backend.
        .with_extension(OtelName("GET /invoices/{id}".into()))
        .send()
        .await?
        .text()
        .await
        .map_err(Into::into)
}
```

`reqwest-middleware` 0.5 is the reqwest 0.13 line; 0.4 is for reqwest 0.12 and will not accept the
client. Injection is unconditional — add the `DisableOtelPropagation` extension for a third party
that should not see internal trace ids. Work handed to `tokio::spawn` leaves the current span
behind: attach it with `.instrument(Span::current())` or the task starts its own trace.

## Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | filter directives; overrides the built-in default |
| `APP__TELEMETRY__OTLP_ENDPOINT` | collector address, passed to `with_endpoint`; `grpc-tonic` talks to port 4317, not 4318. Unset means no exporter at all |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | **ignored**: with the setting unset no exporter is built, with it set `with_endpoint` wins |
| `OTEL_SERVICE_NAME` | `service.name` on every span, read by `Resource::builder()` |
| `OTEL_RESOURCE_ATTRIBUTES` | extra resource attributes, e.g. `deployment.environment=staging` |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | `always_on`, `always_off`, `traceidratio`, `parentbased_*` |
| `OTEL_BSP_SCHEDULE_DELAY` | batch flush interval, default 5000 ms |

## Debugging

**Spans never reach the collector.** Work down the list; each step is cheaper than the next.

1. Is `APP__TELEMETRY__OTLP_ENDPOINT` actually set? `None` builds no exporter at all, and
   `OTEL_EXPORTER_OTLP_ENDPOINT` does not substitute for it — check the environment first.
2. Does `RUST_LOG` still admit the spans? The filter runs before the OTel layer, so `RUST_LOG=warn`
   exports nothing recorded at `info`.
3. Is `guard.shutdown()` reached? A process that exits another way flushes nothing.
4. Is the provider built inside the tokio runtime? A `LazyLock` provider has no reactor.
5. Is the endpoint the gRPC port (4317)? `grpc-tonic` pointed at 4318 fails per batch, not at
   startup, so nothing looks wrong until the first flush.
6. Turn on the SDK's diagnostics with `RUST_LOG=info,opentelemetry=debug`; export errors go there
   and nowhere else.
7. Only then suspect the collector.

**Trace ids differ across services.**

1. `global::set_text_map_propagator(TraceContextPropagator::new())` is missing in the caller, the
   callee, or both. Much the most common cause, and it warns about nothing.
2. The outbound call used a bare `reqwest::Client` instead of `AppState`'s `ClientWithMiddleware`.
3. The callee has no `OtelAxumLayer`, or its route was registered after the layer.
4. The call came from a `tokio::spawn` without `.instrument(Span::current())`.
5. Something in between strips headers — a proxy allowlist or a gateway that rebuilds the request.
6. The two disagree on the format: `TraceContextPropagator` speaks W3C `traceparent`, and a peer
   speaking B3 needs a Zipkin propagator installed on both sides.

## Verifying telemetry in tests

`/metrics` is a real endpoint with a real contract, so it gets an API test (the metrics
reference shows it). Asserting that a span was created is rarely worth it: telemetry is an
observability concern, not a contract, and span assertions break on every refactor while catching
nothing a user would notice. The one exception is a propagation test proving that `traceparent`
survives an inbound-to-outbound hop, because that is the part that fails silently in production.
Default to `#[test_log::test(tokio::test)]` so a failing test prints its own logs, and reach for
`tracing-test`'s `#[traced_test]`, which provides `logs_contain`, only on the few tests that assert
on log content; the two attributes conflict because both install a subscriber.

## Red flags

| About to… | Rule |
|---|---|
| Call `tracing_subscriber::…init()` or `global::set_*` outside `telemetry::init`, or `PrometheusMetricLayer::pair()` outside `build_router` | One wiring point each — the second subscriber silently loses, the second recorder panics |
| Layer a second HTTP tracing layer next to `OtelAxumLayer` | Two server spans per request — the two Otel layers are the only HTTP span source |
| Write `#[instrument]` with no `skip_all` on a function taking a pool, client or credential | Every argument becomes a field |
| `%` a `SecretString`, token or header value | `secrecy` redacts `Debug` only |
| Call `guard.shutdown()` before `axum::serve` returns, or not at all | The spans of in-flight requests are lost |
| Build a `reqwest::Client` for one outbound call | No `traceparent` — use the client in `AppState` |
| Add `.with_sampler(..)`, `.with_service_name(..)` or the `rt-tokio` feature | The first two override env vars; the third has not been the cause since 0.32 |

## Older OpenTelemetry APIs

| Pre-0.32 | Now |
|---|---|
| `global::shutdown_tracer_provider()` | removed in 0.27 — keep the provider and call `provider.shutdown()` |
| `opentelemetry_sdk` feature `rt-tokio` | only the experimental async-runtime processors; the batch processor owns a thread |
| `opentelemetry-otlp` features `["grpc-tonic"]` | with `default-features = false`, list `trace` as well |
| `provider.tracer("x")` | needs `use opentelemetry::trace::TracerProvider as _;` |
| `span.set_parent(cx);` | returns `Result` in 0.33; a bare call trips `unused_must_use` |
| `metrics::counter!("x", 1)` | 0.24 macros return a handle: `counter!("x").increment(1)` |
