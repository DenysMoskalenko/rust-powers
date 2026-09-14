//! The cases that prove the wiring: a create, its conflict, a miss, a list, and
//! the three rejections that answer before the handler ever runs.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test helpers")]

mod common;

use axum::http::StatusCode;
use common::{frozen_now, test_app};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn create_user_returns_201_and_the_created_row() {
    let app = test_app().await;

    let response = app
        .server
        .post("/users")
        .json(&json!({ "email": "ada@example.com", "name": "Ada Lovelace" }))
        .await;

    response.assert_status(StatusCode::CREATED);
    let body: serde_json::Value = response.json();
    assert_eq!(body["email"], "ada@example.com");
    assert_eq!(body["name"], "Ada Lovelace");
    // Proof the injected clock is the one the handler used.
    assert_eq!(body["created_at"], json!(frozen_now()));
    assert!(Uuid::parse_str(body["id"].as_str().unwrap()).is_ok());
}

#[tokio::test]
async fn duplicate_email_returns_409_with_the_error_body() {
    let app = test_app().await;
    let payload = json!({ "email": "ada@example.com", "name": "Ada" });

    app.server
        .post("/users")
        .json(&payload)
        .await
        .assert_status(StatusCode::CREATED);

    let response = app
        .server
        .post("/users")
        .json(&payload)
        .expect_failure()
        .await;

    // The unique index raises 23505; `AppError` turns that into a 409, not a 500.
    response.assert_status(StatusCode::CONFLICT);
    let body: serde_json::Value = response.json();
    // A constant: the driver's own text names the constraint and the table.
    assert_eq!(body["error"], "conflict");
    assert!(body["request_id"].is_string(), "ErrorBody shape: {body}");
}

#[tokio::test]
async fn unknown_user_returns_404() {
    let app = test_app().await;

    let response = app
        .server
        .get(&format!("/users/{}", Uuid::now_v7()))
        .expect_failure()
        .await;

    response.assert_status(StatusCode::NOT_FOUND);
    assert!(response.json::<serde_json::Value>()["error"].is_string());
}

#[tokio::test]
async fn list_users_returns_a_page_envelope() {
    let app = test_app().await;
    for i in 0..3 {
        app.server
            .post("/users")
            .json(&json!({ "email": format!("user{i}@example.com"), "name": "User" }))
            .await
            .assert_status(StatusCode::CREATED);
    }

    let response = app.server.get("/users").add_query_param("limit", 2).await;

    response.assert_status_ok();
    let page: serde_json::Value = response.json();
    assert_eq!(page["total"], 3);
    assert_eq!(page["limit"], 2);
    assert_eq!(page["offset"], 0);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
}

/// Every rejection answers in `ErrorBody`, never in axum's plain text.
fn assert_error_body(body: &serde_json::Value) {
    assert!(body["error"].is_string(), "ErrorBody shape: {body}");
    assert!(body["request_id"].is_string(), "ErrorBody shape: {body}");
}

#[tokio::test]
async fn a_path_segment_that_is_not_a_uuid_returns_400_in_the_error_body() {
    let app = test_app().await;

    // Bare `axum::extract::Path` would answer `Invalid URL: ...` as text/plain;
    // `crate::extract::Path` maps the rejection onto `AppError::BadRequest`.
    let response = app.server.get("/users/not-a-uuid").expect_failure().await;

    response.assert_status(StatusCode::BAD_REQUEST);
    assert_error_body(&response.json());
}

#[tokio::test]
async fn a_body_without_the_json_content_type_returns_415() {
    let app = test_app().await;

    let response = app
        .server
        .post("/users")
        .text(r#"{ "email": "ada@example.com", "name": "Ada" }"#)
        .expect_failure()
        .await;

    // `MissingJsonContentType` carries its own 415; a hand-written 400 would hide it.
    response.assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_error_body(&response.json());
}

#[tokio::test]
async fn a_body_over_the_limit_returns_413() {
    let app = test_app().await;

    let response = app
        .server
        .post("/users")
        // Over `DefaultBodyLimit`'s two megabytes, so the body is never buffered.
        .json(&json!({ "email": "ada@example.com", "name": "a".repeat(3 * 1024 * 1024) }))
        .expect_failure()
        .await;

    response.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    assert_error_body(&response.json());
}
