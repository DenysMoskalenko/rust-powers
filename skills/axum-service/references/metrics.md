# Metrics

Assumes metrics 0.24 (the facade) and axum-prometheus 0.10, which brings
metrics-exporter-prometheus and re-exports the pieces of it you need.

## Contents

- [The recorder](#the-recorder)
- [Default metrics](#default-metrics)
- [Custom metrics](#custom-metrics)
- [Cardinality](#cardinality)
- [Testing the endpoint](#testing-the-endpoint)

## The recorder

The `metrics` recorder is **process-global**: one per process, installed once.
`PrometheusMetricLayer::pair()` installs one every time it is called, so a second call anywhere in
the process panics with `Failed to set global recorder`. That is why the scaffold does not call
it. `build_router` builds the layer with `PrometheusMetricLayerBuilder` and takes the handle from
a `OnceLock`, so the first router installs the recorder and every later one (a second listener,
the next test in the same process) shares it:

```rust,verify
//! The metrics half of `lib.rs`: one recorder per process, one layer per router.
use std::sync::OnceLock;

use axum::Router;
use axum::routing::get;
use axum_prometheus::metrics_exporter_prometheus::PrometheusHandle;
use axum_prometheus::{EndpointLabel, PrometheusMetricLayerBuilder};

/// The `metrics` recorder is one per process by design. It is installed by the
/// first router and shared by every later one (a second listener, the next test
/// in the same process), which would otherwise panic installing it again.
static METRICS: OnceLock<PrometheusHandle> = OnceLock::new();

fn metrics_handle() -> PrometheusHandle {
    METRICS
        .get_or_init(|| axum_prometheus::Handle::default().0)
        .clone()
}

/// A request no route matched has no `MatchedPath`. Labelling it with the raw
/// path, the default, mints one series per URL a scanner tries.
fn unmatched(_path: &str) -> String {
    "unmatched".to_owned()
}

pub fn with_metrics(router: Router) -> Router {
    let (prometheus_layer, metric_handle) = PrometheusMetricLayerBuilder::new()
        .with_endpoint_label_type(EndpointLabel::MatchedPathWithFallbackFn(unmatched))
        .with_metrics_from_fn(metrics_handle)
        .build_pair();

    router
        .route("/metrics", get(move || async move { metric_handle.render() }))
        .layer(prometheus_layer)
}
```

`axum_prometheus::Handle::default()` is what `pair()` runs inside: it builds the recorder with the
crate's duration buckets, installs it, and spawns the upkeep task with `tokio::spawn`. The first
`build_router` call therefore has to happen inside the tokio runtime — in a `LazyLock`, or before
`#[tokio::main]`, it panics, for the same reason the OTLP exporter is built inside `init`.
`build_pair` only exists after `with_default_metrics` or `with_metrics_from_fn`: the builder is a
typestate.

`/metrics` and the probes are counted like every other route. To keep them out of the request
metrics, add `.with_ignore_patterns(&["/metrics", "/health/live", "/health/ready"])` to the builder
rather than moving those routes outside the layers.

For custom buckets, replace `Handle::default()` inside the `OnceLock`, and take over the two
things it did besides building the recorder, installing it and running upkeep:

```rust,verify
use std::sync::OnceLock;
use std::time::Duration;

use axum_prometheus::{
    AXUM_HTTP_REQUESTS_DURATION_SECONDS,
    metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle},
};
use metrics::{Unit, counter, describe_counter, describe_histogram, histogram};

static METRICS: OnceLock<PrometheusHandle> = OnceLock::new();

/// Sub-second buckets for a service whose requests finish in milliseconds.
const BUCKETS: &[f64] = &[0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0];

pub fn metrics_handle() -> PrometheusHandle {
    METRICS.get_or_init(install_recorder).clone()
}

#[expect(
    clippy::expect_used,
    reason = "a bad bucket list or a second recorder is a startup-time programming error"
)]
fn install_recorder() -> PrometheusHandle {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(AXUM_HTTP_REQUESTS_DURATION_SECONDS.to_string()),
            BUCKETS,
        )
        .expect("bucket list is non-empty")
        .install_recorder()
        .expect("no other recorder installed");

    // `install_recorder` leaves upkeep to the caller. Without it histogram
    // samples pile up between scrapes, and without a scraper they never drain.
    let upkeep = handle.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            upkeep.run_upkeep();
        }
    });
    handle
}

/// Once at startup. Without a description the metric still works, but `/metrics`
/// carries no HELP or TYPE line and a dashboard shows a bare name.
pub fn describe() {
    describe_counter!("orders_total", Unit::Count, "orders accepted, by outcome");
    describe_histogram!(
        "db_query_duration_seconds",
        Unit::Seconds,
        "query latency by table"
    );
}

pub fn record_order(outcome: &'static str, elapsed: Duration) {
    // Labels are a bounded set: `outcome` is one of a handful of literals.
    counter!("orders_total", "outcome" => outcome).increment(1);
    histogram!("db_query_duration_seconds", "table" => "orders").record(elapsed);
}
```

Failing loudly at startup is the right behaviour here: a misconfigured recorder that returns an
error would leave the service running with no metrics at all, which nobody notices until an
incident.

## Default metrics

Three, not two:

| Metric | Type | Labels |
|---|---|---|
| `axum_http_requests_total` | counter | `method`, `status`, `endpoint` |
| `axum_http_requests_duration_seconds` | histogram | `method`, `status`, `endpoint` |
| `axum_http_requests_pending` | gauge | `method`, `endpoint` |

`.with_prefix("my_app")` renames the family at runtime. The `AXUM_HTTP_REQUESTS_TOTAL` constants are
compile-time `option_env!` values set in `.cargo/config.toml` under `[env]`, not runtime settings —
changing them needs a rebuild.

The process-level family that every Prometheus dashboard expects (`process_cpu_seconds_total`,
`process_resident_memory_bytes`, open file descriptors) has no Rust equivalent out of the box;
`metrics-process` is the maintained crate that emits it, added when a dashboard needs it.

## Custom metrics

Record through the `metrics` facade, never through `metrics-exporter-prometheus` directly — the
facade is what keeps the call site independent of the exporter. The 0.24 macros return a handle, so
the value goes in a method call: `counter!("orders_total").increment(1)`, not
`counter!("orders_total", 1)`.

Call `describe_*` once at startup. It only adds the `HELP` and `TYPE` lines, but without them the
metric appears with no unit and no explanation, and whoever builds the dashboard guesses.

Names follow the Prometheus convention `<namespace>_<name>_<unit>`: counters end in `_total`,
durations in `_seconds` (never milliseconds — base units only), sizes in `_bytes`.
`histogram!(..).record(x)` takes an `f64` or a `Duration`.

Instrument what a dashboard or an alert will actually read: work completed by outcome, the latency
of anything that leaves the process, and the depth of any queue. A metric nobody plots is a metric
nobody maintains.

## Cardinality

Every distinct combination of label values is a separate time series, kept for the whole retention
window. A Prometheus server dies of them long before the service notices.

**Never a label:** user id, request id, trace id, email address, session token, raw URL path, full
SQL text, error message, or anything else a client controls.

**Fine as a label:** route template, HTTP method, status code, a fixed outcome enum, upstream
service name, queue name. Keep the product of all label-value counts on one metric in the low
thousands.

A value that is forbidden as a label is usually fine as a span field: a trace holds one request, a
metric holds all of them. `fields(user_id = %id)` on `#[instrument]` costs nothing;
`"user_id" => id` in `counter!` is an outage.

`EndpointLabel::MatchedPath` is the default in 0.10, but it falls back to the raw URI path when
`MatchedPath` is unavailable. That is every request no route matched, so each URL a scanner
probes becomes its own series, and it can also happen under nested routers, where
`/users/8f3c…/orders/991` becomes one. The scaffold uses
`EndpointLabel::MatchedPathWithFallbackFn(unmatched)` instead: a request without a matched path is
counted under the one label `unmatched`. Keep the fallback a constant; a function that returns
the path, or a prefix of it, brings the series back.

The scaffold's stack wraps `/metrics` and the health probes like every other route, so scrapes and
probes are counted. Once a ten-second scrape interval dominates the request count or a liveness
probe answering in 30 µs flattens the latency histogram, add `with_ignore_patterns` (above) rather
than registering those routes after the layer, which would also cost them their request id.

## Testing the endpoint

`/metrics` is a real endpoint with a real contract, so it is worth one API test — which is also the
cheapest place to prove the `endpoint` label is a route template and not a raw path.

```rust,verify,test
use axum::http::StatusCode;
use uuid::Uuid;

use crate::common::test_app;

#[tokio::test]
async fn requests_are_counted_under_the_route_template() {
    let app = test_app().await;
    let id = Uuid::now_v7();

    // A 404 from the handler: the route matched, the row does not exist.
    app.server
        .get(&format!("/users/{id}"))
        .expect_failure()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    // A 404 from the fallback: no route matched.
    app.server
        .get("/wp-admin/setup.php")
        .expect_failure()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    let body = app.server.get("/metrics").await.text();

    assert!(
        body.contains(r#"endpoint="/users/{id}""#),
        "expected the route template as the label, got:\n{body}"
    );
    assert!(body.contains(r#"endpoint="unmatched""#), "{body}");
    // Neither the uuid nor the scanner's path: one series per value kills Prometheus.
    assert!(!body.contains(&id.to_string()), "raw path leaked into a label");
    assert!(!body.contains("wp-admin"), "raw path leaked into a label");
}
```

The test goes through `test_app()` and the real router, and asserts that a label is present, not
how many requests it counted. Both follow from the recorder being process-wide: under plain
`cargo test` every test in the binary shares it, so counts include other tests' requests, and a
test that builds its own router with `PrometheusMetricLayer::pair()` panics with
`Failed to set global recorder` as soon as another test in the same process has built one (under
nextest, one process per test, it would pass and hide the problem). For test structure and
fixtures see `rust-testing`.
