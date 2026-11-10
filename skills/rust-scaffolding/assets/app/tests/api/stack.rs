//! The middleware stack, tested through the real router: what the request id
//! accepts, what a timeout answers, how an unmatched path is counted, and that
//! an inbound trace is continued rather than replaced.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test helpers")]

use std::sync::LazyLock;

use app::api::{request_id, request_timeout};
use app::config::REQUEST_TIMEOUT;
use app::{REQUEST_ID_HEADER, build_router};
use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use axum_test::TestServer;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tower::ServiceBuilder;
use tracing::Dispatch;
use tracing_subscriber::layer::SubscriberExt as _;
use uuid::Uuid;

use crate::common::test_app;

#[tokio::test]
async fn an_inbound_request_id_is_kept_when_it_looks_like_one() {
    let app = test_app().await;

    let response = app
        .server
        .get("/version")
        .add_header(REQUEST_ID_HEADER, "req-42_a.b")
        .await;

    response.assert_status_ok();
    assert_eq!(response.header(REQUEST_ID_HEADER), "req-42_a.b");
}

#[tokio::test]
async fn an_inbound_request_id_that_is_not_id_shaped_is_replaced() {
    let app = test_app().await;

    for hostile in ["a".repeat(65), "two words".to_owned(), "a\"b".to_owned()] {
        let response = app
            .server
            .get("/version")
            .add_header(REQUEST_ID_HEADER, hostile.as_str())
            .await;

        response.assert_status_ok();
        let minted = response.header(REQUEST_ID_HEADER);
        // A uuid the service minted, not the client's text.
        Uuid::parse_str(minted.to_str().unwrap()).unwrap();
    }
}

async fn slow() -> &'static str {
    tokio::time::sleep(REQUEST_TIMEOUT * 2).await;
    "late"
}

// Paused time is safe here: this router has no database, so nothing waits on
// real I/O while the clock jumps to the timeout.
#[tokio::test(start_paused = true)]
async fn a_handler_past_the_budget_answers_503_in_the_error_body() {
    let router = Router::new().route("/slow", get(slow)).layer(
        ServiceBuilder::new()
            .layer(axum::middleware::from_fn(request_id))
            .layer(axum::middleware::from_fn(request_timeout)),
    );
    let server = TestServer::new(router);

    let response = server.get("/slow").expect_failure().await;

    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    let body: serde_json::Value = response.json();
    assert_eq!(body["error"], "service unavailable");
    assert!(body["request_id"].is_string(), "ErrorBody shape: {body}");
}

#[tokio::test]
async fn the_router_can_be_built_more_than_once_in_a_process() {
    let app = test_app().await;

    // The metrics recorder is process-wide; a second router must reuse it.
    let second = TestServer::new(build_router(app.state.clone()));

    second.get("/health/live").await.assert_status_ok();
}

#[tokio::test]
async fn unmatched_paths_share_one_metric_label() {
    let app = test_app().await;
    app.server
        .get("/scanner/probe-1")
        .expect_failure()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    app.server.get("/health/live").await.assert_status_ok();

    let metrics = app.server.get("/metrics").await.text();

    assert!(metrics.contains(r#"endpoint="/health/live""#), "{metrics}");
    assert!(metrics.contains(r#"endpoint="unmatched""#), "{metrics}");
    // The raw path as a label is one series per URL a scanner tries.
    assert!(!metrics.contains("probe-1"), "{metrics}");
}

const TRACE_ID: &str = "0af7651916cd43dd8448eb211c80319c";
const CALLER_SPAN_ID: &str = "b7ad6b7169203331";

/// Every span the process finishes, and the subscriber that collects them. It
/// is installed globally, not only as a thread-local default: under
/// `cargo test` the tests share one process, and `tracing` caches whether a
/// span is enabled per call site, so a scoped subscriber that is the only one
/// loses a race against the first test thread that has none.
static TRACING: LazyLock<(InMemorySpanExporter, Dispatch)> = LazyLock::new(|| {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_text_map_propagator(TraceContextPropagator::new());
    let dispatch = Dispatch::new(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test"))),
    );
    // Refused when a `#[test_log::test]` took the global slot first. The test
    // then relies on its thread-local default, which is safe with a second
    // subscriber registered: the call-site cache asks both.
    let _ = tracing::dispatcher::set_global_default(dispatch.clone());
    (exporter, dispatch)
});

#[tokio::test]
async fn an_inbound_trace_is_continued_and_echoed() {
    let (spans, dispatch) = &*TRACING;
    let _subscriber = tracing::dispatcher::set_default(dispatch);
    let app = test_app().await;

    let response = app
        .server
        .get("/version")
        .add_header("traceparent", format!("00-{TRACE_ID}-{CALLER_SPAN_ID}-01"))
        .add_header(REQUEST_ID_HEADER, "trace-test")
        .await;

    response.assert_status_ok();
    let echoed = response.header("traceparent");
    let echoed = echoed.to_str().unwrap();
    // The caller's trace, and the server span's id rather than the caller's own.
    assert!(echoed.starts_with(&format!("00-{TRACE_ID}-")), "{echoed}");
    assert!(!echoed.contains(CALLER_SPAN_ID), "{echoed}");

    // The `request` span joined that trace too: it is a child of the server
    // span, not the root of a second trace.
    let spans = spans.get_finished_spans().unwrap();
    let request = spans
        .iter()
        .find(|span| {
            span.attributes.iter().any(|attribute| {
                attribute.key.as_str() == "request_id" && attribute.value.as_str() == "trace-test"
            })
        })
        .expect("the `request` span was exported");
    assert_eq!(request.span_context.trace_id().to_string(), TRACE_ID);
}
