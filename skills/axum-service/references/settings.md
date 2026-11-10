# Settings, secrets and .env

- [The Settings struct](#the-settings-struct)
- [Testing settings](#testing-settings)
- [dotenv precedence](#dotenv-precedence)
- [Secrets](#secrets)
- [Reaching settings from a handler](#reaching-settings-from-a-handler)

Configuration comes from the environment only — no config files. `config` is pinned with
`default-features = false`, which removes every file-format parser, so a `config::File` source
still compiles but fails at `build()` with a misleading `configuration file … not found`, even
when the file is there: with no format enabled, no extension matches. There is no file source in
this stack.

## The Settings struct

```rust,verify
//! `config.rs` — typed settings. Environment variables only, no
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
```

Every value reaches serde as a string, and serde parses it into the field's type: `port: u16`
from `"9000"`, `json_logs: bool` from `"false"`. `config`'s `try_parsing(true)` is not needed for
that and is not used, because it re-types every value that looks like a number or a bool before
serde sees it: a secret `0012345` would arrive as `12345`. A list field is the one case a string
does not cover, so it takes `#[serde(deserialize_with = "comma_separated")]` instead of
`try_parsing` with `list_separator` and `with_list_parse_key`. An empty variable is an empty list.
In a project that keeps the `config` list options, the key given to `with_list_parse_key`
(`server.cors_origins`) is the lower-case dotted path after the prefix, not the variable name, and
`list_separator` does nothing unless `try_parsing` is on.

`build` then checks what serde cannot: every CORS origin must be `scheme://host[:port]` with no
path and no trailing slash, the exact string a browser sends in `Origin`, and anything else fails
at startup with the variable named in the error. A check that spans fields, or one whose failure
would otherwise show only in production, belongs in `build` the same way.

A sub-struct whose every field has a default never materialises on its own — with
no `APP__SERVER__*` variable at all, deserialization fails with "missing field server" — which is
what `#[serde(default)]` on the `server` and `telemetry` fields is for, paired with a hand-written
`impl Default` for the struct. The alternative is seeding an empty table with
`Config::builder().set_default("server", ..)`; pick one. A new area (`AuthSettings.token_ttl_secs`,
say) is a field with `#[serde(default = "..")]` next to the ones that need it, read from
`APP__AUTH__TOKEN_TTL_SECS`.

## Testing settings

Contract tests for the parsing rules, against an in-memory map. For test structure and fixtures see
`rust-testing`.

```rust,verify,test
//! Settings tests build the map instead of touching the process environment.
use std::collections::HashMap;

use app::config::Settings;
use secrecy::ExposeSecret;

fn base() -> HashMap<String, String> {
    HashMap::from([
        ("APP__DATABASE__URL".to_owned(), "postgres://localhost/app".to_owned()),
        ("APP__AUTH__JWT_SECRET".to_owned(), "test-secret-of-at-least-32-bytes!".to_owned()),
    ])
}

#[test]
fn defaults_apply_and_values_parse() {
    let mut env = base();
    env.insert("APP__SERVER__PORT".to_owned(), "9999".to_owned());

    let settings = Settings::from_map(env).expect("settings load");

    assert_eq!(settings.server.port, 9999);
    assert_eq!(settings.server.host, "0.0.0.0", "serde default applies");
    assert_eq!(settings.database.url.expose_secret(), "postgres://localhost/app");
}

#[test]
fn a_numeric_looking_secret_and_a_list_arrive_as_written() {
    let mut env = base();
    env.insert("APP__AUTH__JWT_SECRET".to_owned(), "0012345".to_owned());
    env.insert(
        "APP__SERVER__CORS_ORIGINS".to_owned(),
        "https://a.example, http://localhost:5173".to_owned(),
    );

    let settings = Settings::from_map(env).expect("settings load");

    assert_eq!(settings.auth.jwt_secret.expose_secret(), "0012345");
    assert_eq!(
        settings.server.cors_origins,
        ["https://a.example", "http://localhost:5173"]
    );
}

#[test]
fn an_origin_with_a_path_fails_at_load() {
    let mut env = base();
    env.insert(
        "APP__SERVER__CORS_ORIGINS".to_owned(),
        "https://a.example/".to_owned(),
    );

    let err = Settings::from_map(env).expect_err("a trailing slash is not an origin");

    assert!(err.to_string().contains("is not an origin"), "got: {err}");
}

#[test]
fn a_missing_required_key_is_an_error() {
    let mut env = base();
    env.remove("APP__AUTH__JWT_SECRET");

    let err = Settings::from_map(env).expect_err("jwt_secret is required");

    assert!(format!("{err}").contains("auth"), "got: {err}");
}

#[test]
fn secrets_are_redacted_in_debug_output() {
    let settings = Settings::from_map(base()).expect("settings load");

    assert!(!format!("{settings:?}").contains("test-secret-of-at-least-32-bytes!"));
}
```

## dotenv precedence

`dotenvy::dotenv().ok()` is the first statement of `main`, before `Settings::load()`. It never
overwrites a variable that is already set, so a real environment variable always wins and a
missing `.env` is a no-op — production reads the real environment only. Keep `.env` for local
development, commit `.env.example` with placeholder values, and never commit `.env`.

## Secrets

Every password, token and connection URL is a `SecretString` (`secrecy` with the `serde`
feature). Its `Debug` prints a redaction, so logging the whole `Settings` struct is safe; read the
value with `.expose_secret()` at the single point of use and never bind it to a longer-lived
`String`. In secrecy 0.10 build one with `SecretString::from(s)`; `Secret::new` is the 0.8 API.

## Reaching settings from a handler

Put `Arc<Settings>` in `AppState` and read it through `State`. A handler that needs one field is
cleaner with a `FromRef` impl for that field's type than with a fresh `Settings::load()` call —
loading re-reads the environment on every request.
