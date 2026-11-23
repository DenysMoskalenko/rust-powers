#!/usr/bin/env bash
set -euo pipefail
mkdir src
cat > Cargo.toml <<'TOML'
[package]
name = "ledger-api"
version = "0.1.0"
edition = "2024"

[dependencies]
axum = "0.8"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt"] }
tower = { version = "0.5", features = ["util"] }
TOML
cat > src/lib.rs <<'RS'
use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};

pub const BATCH_LIMIT: u32 = 37;

pub enum LedgerError {
    BatchTooLarge,
}

impl IntoResponse for LedgerError {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::UNPROCESSABLE_ENTITY, "ledger.batch_too_large").into_response()
    }
}

pub fn router() -> Router {
    Router::new().route("/batch-limit", get(async || BATCH_LIMIT.to_string()))
}
RS
