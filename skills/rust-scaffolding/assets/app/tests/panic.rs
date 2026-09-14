//! A handler panic is a bug, but it must still answer: the constant 500 body
//! carrying the request id, on a connection that stays open. The scaffold has no
//! panicking route, so the layer is wired around one here, in the same order as
//! `build_router`: request id outside, panic catcher directly inside.
#![allow(clippy::panic, reason = "the panicking handler is the point")]

use app::REQUEST_ID_HEADER;
use app::api::request_id;
use app::error::catch_panic_layer;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use axum_test::TestServer;
use tower::ServiceBuilder;

// Edition 2024 infers `!` for a closure that only panics, and `!` is not a
// response; a named handler pins the type.
async fn boom() -> &'static str {
    panic!("boom")
}

#[tokio::test]
async fn a_panicking_handler_answers_the_constant_500_body() {
    let router = Router::new().route("/boom", get(boom)).layer(
        ServiceBuilder::new()
            .layer(axum::middleware::from_fn(request_id))
            .layer(catch_panic_layer()),
    );
    let server = TestServer::new(router);

    let response = server.get("/boom").expect_failure().await;

    response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    let body: serde_json::Value = response.json();
    assert_eq!(body["error"], "internal server error");
    // The message never reaches the client, only the log.
    assert!(!body.to_string().contains("boom"), "leaked: {body}");
    assert!(body["request_id"].is_string(), "ErrorBody shape: {body}");
    assert!(response.headers().contains_key(REQUEST_ID_HEADER));
}
