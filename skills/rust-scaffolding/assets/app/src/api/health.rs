//! The three endpoints an orchestrator asks for. Keep them free of business
//! logic: a readiness probe that runs a real query will flap.
use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::config::READINESS_TIMEOUT;

#[derive(Debug, Serialize, ToSchema)]
pub struct Version {
    pub name: String,
    pub version: String,
}

/// One dependency's answer. Ordered worst-last so the overall status is `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Ok,
    /// An optional dependency (a cache, a broker) is down; requests still work.
    Degraded,
    /// A required dependency (the database) is down; take this instance out.
    Unavailable,
}

/// The readiness body: the worst check decides `status`, and `Unavailable` is
/// the only value that turns the probe into a 503.
#[derive(Debug, Serialize, ToSchema)]
pub struct Readiness {
    pub status: Health,
    /// One entry per dependency, named after the thing checked.
    pub checks: BTreeMap<String, Health>,
}

/// Liveness: the process is running. Never touches a dependency, or Kubernetes
/// restarts the pod every time the database hiccups.
#[utoipa::path(get, path = "/health/live", tag = "health", responses((status = 200)))]
pub async fn live() -> StatusCode {
    StatusCode::OK
}

/// Readiness: this instance can serve traffic. A failure takes it out of the load
/// balancer without killing it.
#[utoipa::path(
    get, path = "/health/ready", tag = "health",
    responses(
        (status = 200, body = Readiness),
        (status = 503, body = Readiness, description = "A required dependency is unreachable"),
    )
)]
// House style: `skip_all` plus the fields worth having. `skip(state)` would still
// try to record every other argument, which is how a password reaches a log.
#[tracing::instrument(skip_all)]
pub async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    // One line per dependency. A required one answers `Unavailable` when it
    // fails; an optional one (cache, messaging) answers `Degraded` instead and
    // leaves the probe at 200.
    let checks = BTreeMap::from([("database".to_owned(), database(&state.db).await)]);

    let status = checks.values().copied().max().unwrap_or(Health::Ok);
    let code = if status == Health::Unavailable {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (code, Json(Readiness { status, checks }))
}

async fn database(db: &sea_orm::DatabaseConnection) -> Health {
    // A ping that hangs is a failed probe, not a probe that never answers: without
    // the timeout the orchestrator's own deadline decides, and the pod looks dead.
    let reason = match tokio::time::timeout(READINESS_TIMEOUT, db.ping()).await {
        Ok(Ok(())) => return Health::Ok,
        Ok(Err(error)) => format!("ping failed: {error}"),
        Err(_) => format!("ping timed out after {READINESS_TIMEOUT:?}"),
    };
    tracing::warn!(%reason, "readiness: database unavailable");
    Health::Unavailable
}

/// Build info, so a running pod can be matched to a commit.
#[utoipa::path(get, path = "/version", tag = "health", responses((status = 200, body = Version)))]
pub async fn version() -> Json<Version> {
    Json(Version {
        name: env!("CARGO_PKG_NAME").to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(live))
        .routes(routes!(ready))
        .routes(routes!(version))
}
