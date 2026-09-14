//! Settings: typed configuration. Environment variables only, no
//! config files. `APP__DATABASE__URL` becomes `settings.database.url`.
use secrecy::SecretString;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

// The timeout budget, in one place so the numbers can be compared. Each one is
// shorter than the layer above it: a handler that has given up waiting on
// Postgres still has time to render an error before the request timeout fires.
/// Whole inbound request, enforced by the outermost `TimeoutLayer`.
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

impl Settings {
    /// Reads the process environment: `APP__SERVER__PORT`, `APP__DATABASE__URL`, ...
    ///
    /// # Errors
    /// A required key is missing, or a value does not parse into its type.
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
        config::Config::builder()
            .add_source(
                source
                    // `__` twice: once between the prefix and the first key, once
                    // between nested keys. A single `_` would split `max_connections`.
                    .prefix_separator("__")
                    .separator("__")
                    // Without this every value stays a String and `port: u16` fails.
                    .try_parsing(true)
                    // `Vec<String>` fields need both, and the key is named after
                    // the deserialised path, not the environment variable.
                    .list_separator(",")
                    .with_list_parse_key("server.cors_origins"),
            )
            .build()?
            .try_deserialize()
    }
}
