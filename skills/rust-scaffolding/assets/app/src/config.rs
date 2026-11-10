//! Settings: typed configuration. Environment variables only, no
//! config files. `APP__DATABASE__URL` becomes `settings.database.url`.
use std::collections::HashMap;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;

// The timeout budget, in one place so the numbers can be compared. Each one is
// shorter than the layer above it: a handler that has given up waiting on
// Postgres still has time to render an error before the request timeout fires.
/// After SIGTERM, how long in-flight requests get before the process stops
/// waiting for them. Longer than `REQUEST_TIMEOUT`, so a request that arrived
/// just before the signal can finish; the orchestrator's grace period
/// (Kubernetes `terminationGracePeriodSeconds`, 30 by default) goes above it.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(35);
/// Whole inbound request, enforced by `api::request_timeout`, the innermost layer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// One outbound call, including connect.
pub const HTTP_CLIENT_TIMEOUT: Duration = Duration::from_secs(10);
/// Postgres cancels the statement itself, even if the client stopped waiting.
pub const DB_STATEMENT_TIMEOUT: Duration = Duration::from_secs(10);
/// A readiness probe that waits longer than this is already unhealthy.
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    /// `#[serde(default)]` on the field covers "no `APP__SERVER__*` var at all";
    /// the same attribute on the struct covers a partially populated table.
    #[serde(default)]
    pub server: ServerSettings,
    pub database: DatabaseSettings,
    #[serde(default)]
    pub telemetry: TelemetrySettings,
    pub auth: AuthSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    /// Comma-separated in the environment: `APP__SERVER__CORS_ORIGINS=https://a,https://b`.
    /// Empty means no CORS layer at all, which is same-origin only.
    #[serde(deserialize_with = "comma_separated")]
    pub cors_origins: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseSettings {
    /// `SecretString` redacts itself in `Debug`; read it with `expose_secret()`.
    pub url: SecretString,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TelemetrySettings {
    /// `None` disables the OTLP exporter entirely, which is what tests want.
    pub otlp_endpoint: Option<String>,
    pub json_logs: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthSettings {
    pub jwt_secret: SecretString,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_owned(),
            port: 8000,
            cors_origins: Vec::new(),
        }
    }
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            otlp_endpoint: None,
            json_logs: true,
        }
    }
}

fn default_max_connections() -> u32 {
    10
}

/// A list arrives as one string. It is split here, not with `config`'s
/// `try_parsing` and `list_separator`: `try_parsing` also re-types every other
/// value that looks like a number or a bool, so a secret `0012345` would reach
/// the service as `12345`.
fn comma_separated<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect())
}

/// `scheme://host[:port]` and nothing after it. A browser sends exactly that in
/// `Origin`, so `https://app.example.com/` would be configured and never match.
fn is_origin(origin: &str) -> bool {
    !origin.ends_with('/')
        && origin.parse::<axum::http::Uri>().is_ok_and(|uri| {
            uri.scheme().is_some()
                && uri.authority().is_some()
                && uri.path() == "/"
                && uri.query().is_none()
        })
}

impl Settings {
    /// Reads the process environment: `APP__SERVER__PORT`, `APP__DATABASE__URL`, ...
    ///
    /// # Errors
    /// A required key is missing, a value does not parse into its type, or a
    /// CORS origin is not an origin.
    pub fn load() -> Result<Self, config::ConfigError> {
        Self::build(config::Environment::with_prefix("APP"))
    }

    /// The same parsing against an in-memory map, with the same `APP__` keys.
    /// Tests use this instead of `std::env::set_var`, which is `unsafe` in
    /// edition 2024 and therefore unreachable under `unsafe_code = "forbid"`.
    ///
    /// # Errors
    /// Same as [`Settings::load`].
    pub fn from_map(map: HashMap<String, String>) -> Result<Self, config::ConfigError> {
        Self::build(config::Environment::with_prefix("APP").source(Some(map)))
    }

    fn build(source: config::Environment) -> Result<Self, config::ConfigError> {
        let settings: Self = config::Config::builder()
            .add_source(source
                    // `__` twice: once between the prefix and the first key, once
                    // between nested keys. A single `_` would split `max_connections`.
                    // Every value stays a string; serde parses `port: u16` and
                    // `json_logs: bool` from it, and a secret arrives untouched.
                    .prefix_separator("__")
                    .separator("__"))
            .build()?
            .try_deserialize()?;

        // Fail at startup: a mistyped origin otherwise fails only in a browser.
        if let Some(origin) = settings
            .server
            .cors_origins
            .iter()
            .find(|origin| !is_origin(origin))
        {
            return Err(config::ConfigError::Message(format!(
                "APP__SERVER__CORS_ORIGINS: `{origin}` is not an origin; write scheme://host[:port] with no path"
            )));
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret as _;

    use super::*;

    fn settings(extra: &[(&str, &str)]) -> Result<Settings, config::ConfigError> {
        let mut map = HashMap::from([
            (
                "APP__DATABASE__URL".to_owned(),
                "postgres://db/app".to_owned(),
            ),
            ("APP__AUTH__JWT_SECRET".to_owned(), "0012345".to_owned()),
        ]);
        map.extend(
            extra
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        Settings::from_map(map)
    }

    #[test]
    fn a_numeric_looking_secret_arrives_unchanged() {
        let settings = settings(&[]).unwrap();

        assert_eq!(settings.auth.jwt_secret.expose_secret(), "0012345");
    }

    #[test]
    fn numbers_bools_and_lists_parse_from_strings() {
        let settings = settings(&[
            ("APP__SERVER__PORT", "9000"),
            ("APP__TELEMETRY__JSON_LOGS", "false"),
            (
                "APP__SERVER__CORS_ORIGINS",
                "https://a.example, http://localhost:5173,",
            ),
        ])
        .unwrap();

        assert_eq!(settings.server.port, 9000);
        assert!(!settings.telemetry.json_logs);
        assert_eq!(
            settings.server.cors_origins,
            ["https://a.example", "http://localhost:5173"]
        );
    }

    #[test]
    fn an_empty_origin_list_means_no_origins() {
        let settings = settings(&[("APP__SERVER__CORS_ORIGINS", "")]).unwrap();

        assert_eq!(settings.server.cors_origins, Vec::<String>::new());
    }

    #[test]
    fn an_origin_with_anything_after_the_host_fails_at_load() {
        for origin in [
            "https://a.example/",
            "https://a.example/app",
            "https://a.example?x=1",
        ] {
            let error = settings(&[("APP__SERVER__CORS_ORIGINS", origin)]).unwrap_err();

            assert!(error.to_string().contains("is not an origin"), "{error}");
        }
    }
}
