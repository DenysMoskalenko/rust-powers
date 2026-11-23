use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use ledger_api::{BATCH_LIMIT, LedgerError, router};
use tower::ServiceExt as _;

#[tokio::test]
async fn serves_batches_and_keeps_the_ledger_contract() {
    let error = match LedgerError::BatchTooLarge {
        LedgerError::BatchTooLarge => LedgerError::BatchTooLarge.into_response(),
    };
    assert_eq!(BATCH_LIMIT, 37);
    assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
    for (method, path, status, body) in [
        ("GET", "/batch-limit", 200, "37"),
        ("POST", "/batch/1", 204, ""),
        ("POST", "/batch/37", 204, ""),
        ("POST", "/batch/38", 422, "ledger.batch_too_large"),
    ] {
        let request = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        let response = router().oneshot(request).await.unwrap();
        assert_eq!(response.status().as_u16(), status, "{method} {path}");
        assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), body, "{method} {path}");
    }
}
