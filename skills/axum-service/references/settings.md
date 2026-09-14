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
```

The list key (`server.cors_origins`) is the lower-case dotted path after the prefix, not the
variable name. A sub-struct whose every field has a default never materialises on its own — with
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
        ("APP__AUTH__JWT_SECRET".to_owned(), "test-secret".to_owned()),
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
fn a_missing_required_key_is_an_error() {
    let mut env = base();
    env.remove("APP__AUTH__JWT_SECRET");

    let err = Settings::from_map(env).expect_err("jwt_secret is required");

    assert!(format!("{err}").contains("auth"), "got: {err}");
}

#[test]
fn secrets_are_redacted_in_debug_output() {
    let settings = Settings::from_map(base()).expect("settings load");

    assert!(!format!("{settings:?}").contains("test-secret"));
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
