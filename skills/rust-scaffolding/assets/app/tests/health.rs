//! The probes an orchestrator calls. Cheap to test and easy to break: a readiness
//! handler that grows a real query stops being a probe and starts being an outage.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test helpers")]

mod common;

use axum::http::StatusCode;
use common::test_app;
use serde_json::json;

#[tokio::test]
async fn liveness_is_ok_and_touches_nothing() {
    let app = test_app().await;

    app.server.get("/health/live").await.assert_status_ok();
}

#[tokio::test]
async fn readiness_is_ok_while_the_database_answers() {
    let app = test_app().await;

    let response = app.server.get("/health/ready").await;

    response.assert_status_ok();
    // The body is a contract: add-on dependencies add an entry under `checks`.
    response.assert_json(&json!({ "status": "ok", "checks": { "database": "ok" } }));
}

#[tokio::test]
async fn readiness_is_503_once_the_pool_is_closed() {
    let app = test_app().await;
    // The closest thing to "Postgres went away" that a test can arrange.
    app.state.db.close_by_ref().await.unwrap();

    let response = app.server.get("/health/ready").expect_failure().await;

    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    response.assert_json(&json!({
        "status": "unavailable",
        "checks": { "database": "unavailable" },
    }));
}

#[tokio::test]
async fn version_reports_the_crate_name_and_version() {
    let app = test_app().await;

    let response = app.server.get("/version").await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["name"], env!("CARGO_PKG_NAME"));
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn unknown_route_answers_in_the_error_body_shape() {
    let app = test_app().await;

    let response = app.server.get("/nope").expect_failure().await;

    response.assert_status(StatusCode::NOT_FOUND);
    let body: serde_json::Value = response.json();
    assert!(body["error"].is_string(), "ErrorBody shape: {body}");
    // The middleware mints an id even when no handler runs.
    assert!(body["request_id"].is_string(), "ErrorBody shape: {body}");
}

#[tokio::test]
async fn wrong_method_answers_405_in_the_error_body_shape() {
    let app = test_app().await;

    let response = app.server.delete("/version").expect_failure().await;

    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        response.json::<serde_json::Value>()["error"],
        "method not allowed"
    );
}
