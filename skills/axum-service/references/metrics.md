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

`PrometheusMetricLayer::pair()` installs a **process-global** recorder and returns the tower layer
plus the handle that renders the scrape body. The scaffold calls it exactly once, at the top of
`build_router`, and mounts `/metrics` from the handle there. A second call
panics with `Failed to set global recorder`. `pair()` also spawns the recorder's upkeep task with
`tokio::spawn`, so it must run inside the tokio runtime — a `LazyLock` or a call before
`#[tokio::main]` panics too, for the same reason the OTLP exporter is built inside `init`.

`PrometheusMetricLayer::pair()` is the whole configuration for most services, and it is what the
scaffold uses: `/metrics` and the probes are counted like every other route. Use the builder when
you need ignore patterns or custom buckets; it replaces the `pair()` call in `build_router`, and
`with_ignore_patterns` is how to keep `/metrics` and the probes out of the request metrics without
moving those routes outside the layers:

```rust,verify
use std::time::Duration;

use axum_prometheus::{
    AXUM_HTTP_REQUESTS_DURATION_SECONDS, EndpointLabel, PrometheusMetricLayer,
    PrometheusMetricLayerBuilder,
    metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle},
    utils::SECONDS_DURATION_BUCKETS,
};
use metrics::{Unit, counter, describe_counter, describe_histogram, histogram};

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

/// The configured form. `build_pair` only exists after `with_default_metrics`
/// or `with_metrics_from_fn` — the builder is a typestate.
#[expect(
    clippy::expect_used,
    reason = "a bad bucket or a second recorder is a startup-time programming error"
)]
pub fn metrics_pair() -> (PrometheusMetricLayer<'static>, PrometheusHandle) {
    PrometheusMetricLayerBuilder::new()
        .with_ignore_patterns(&["/metrics", "/health/live", "/health/ready"])
        .with_endpoint_label_type(EndpointLabel::MatchedPath)
        .with_metrics_from_fn(|| {
            PrometheusBuilder::new()
                .set_buckets_for_metric(
                    Matcher::Full(AXUM_HTTP_REQUESTS_DURATION_SECONDS.to_string()),
                    SECONDS_DURATION_BUCKETS,
                )
                .expect("bucket list is non-empty")
                .install_recorder()
                .expect("no other recorder installed")
        })
        .build_pair()
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

`EndpointLabel::MatchedPath` is the default in 0.10, but it falls back to `EndpointLabel::Exact`
when `MatchedPath` is unavailable — typically under nested routers. `Exact` is the raw URI path, so
`/users/8f3c…/orders/991` becomes its own series. If the router nests, assert the label in a test
(below) or use `EndpointLabel::MatchedPathWithFallbackFn`.

The scaffold's stack wraps `/metrics` and the health probes like every other route, so scrapes and
probes are counted. Once a ten-second scrape interval dominates the request count or a liveness
probe answering in 30 µs flattens the latency histogram, add `with_ignore_patterns` (above) rather
than registering those routes after the layer, which would also cost them their request id.

## Testing the endpoint

`/metrics` is a real endpoint with a real contract, so it is worth one API test — which is also the
cheapest place to prove the `endpoint` label is a route template and not a raw path.

```rust,verify,test
use axum::{Router, routing::get};
use axum_prometheus::PrometheusMetricLayer;
use axum_test::TestServer;

#[tokio::test]
async fn metrics_endpoint_counts_requests_under_the_route_template() -> anyhow::Result<()> {
    let (layer, handle) = PrometheusMetricLayer::pair();
    let app = Router::new()
        .route("/users/{id}", get(async || "{}"))
        .layer(layer)
        // Registered after the layer, so the scrape does not count itself.
        .route("/metrics", get(async move || handle.render()));
    let server = TestServer::new(app);

    server
        .get("/users/0199a1b2-c3d4-7000-8000-000000000000")
        .await
        .assert_status_ok();

    let body = server.get("/metrics").await.text();

    assert!(
        body.contains(
            r#"axum_http_requests_total{method="GET",status="200",endpoint="/users/{id}"} 1"#
        ),
        "expected one counted request under the route template, got:\n{body}"
    );
    // The uuid must never appear: one series per user id kills Prometheus.
    assert!(!body.contains("0199a1b2"), "raw path leaked into a label");
    Ok(())
}
```

`PrometheusMetricLayer::pair()` installs a process-global recorder, so the second test in one
process to call it panics with `Failed to set global recorder`. Every test that builds the real
router through `test_app()` calls it, which is why the suite runs under nextest (one process per
test) and fails under plain `cargo test` from the second test in a binary onwards. For test
structure and fixtures see `rust-testing`.
