# Axum Integration

Read this file when a Rig agent has to live inside an axum service: held in state, called
from a handler, streamed to a browser, tested offline, and traced.

Verified against `rig` 0.42.0, axum 0.8, axum-test 21. The agent adds one sub-struct to
`Settings`, one field to `AppState`, one module, two routes in `build_router` and one
parameter to `test_app()`; `error.rs` is not touched. This
file shows those deltas and names what stays as it is. For routes, extractors, the error
type itself, SSE transport rules and OpenAPI see `axum-service`.
`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [Settings](#settings)
- [Holding the Agent in `AppState`](#holding-the-agent-in-appstate)
- [Mapping `PromptError` into `AppError`](#mapping-prompterror-into-apperror)
- [The Chat Module](#the-chat-module)
- [Router Wiring](#router-wiring)
- [Notes on the Handlers](#notes-on-the-handlers)
- [Testing with `axum-test` and `MockCompletionModel`](#testing-with-axum-test-and-mockcompletionmodel)
- [OpenTelemetry](#opentelemetry)

## Settings

`Client::from_env()` is the quick-start form. In the service the key is a setting like every
other secret: one more sub-struct on `Settings`, read from `APP__AGENT__*` by the same
`config` source the scaffold already has (`Settings::from_map` in tests), `SecretString` so
it never prints, exposed once where the client is built.

```rust,verify,test
//! `config.rs` delta, and the `build_agent` that `main` calls with it.
use std::collections::HashMap;

use rig::agent::Agent;
use rig::prelude::*;
use rig::providers::{anthropic, gemini, openai};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

/// The scaffold's `Settings` with `agent` added; the other fields are unchanged.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub agent: AgentSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentSettings {
    /// `APP__AGENT__PROVIDER=anthropic`
    pub provider: Provider,
    /// `APP__AGENT__MODEL`: a plain id, verified against the provider's catalogue.
    pub model: String,
    /// `APP__AGENT__API_KEY`. `SecretString` redacts `Debug`; `expose_secret()` once, below.
    pub api_key: SecretString,
}

/// A fixed set is an enum, never a `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    OpenAi,
    Anthropic,
    Gemini,
}

/// Built once in `main`, held as `Arc<Agent>` in `AppState`. Every arm yields the
/// same `AgentBuilder`: the model type is erased at `.agent(..)`.
pub fn build_agent(settings: &AgentSettings) -> anyhow::Result<Agent> {
    let key = settings.api_key.expose_secret();
    let builder = match settings.provider {
        Provider::OpenAi => openai::Client::new(key)?.agent(&settings.model),
        Provider::Anthropic => anthropic::Client::new(key)?.agent(&settings.model),
        Provider::Gemini => gemini::Client::new(key)?.agent(&settings.model),
    };
    Ok(builder.name("chat").preamble("You are terse.").build())
}

#[test]
fn agent_settings_parse_like_every_other_sub_struct() {
    // What `Settings::from_map` does in the scaffold, spelled out.
    let map = HashMap::from([
        ("APP__AGENT__PROVIDER".to_owned(), "anthropic".to_owned()),
        ("APP__AGENT__MODEL".to_owned(), "claude-sonnet-4-6".to_owned()),
        ("APP__AGENT__API_KEY".to_owned(), "sk-test".to_owned()),
    ]);
    let settings: Settings = config::Config::builder()
        .add_source(
            config::Environment::with_prefix("APP")
                .source(Some(map))
                .prefix_separator("__")
                .separator("__")
                .try_parsing(true),
        )
        .build()
        .unwrap()
        .try_deserialize()
        .unwrap();

    assert_eq!(settings.agent.provider, Provider::Anthropic);
    // Building a client is offline; only a prompt talks to the provider.
    assert!(build_agent(&settings.agent).is_ok());
}
```

## Holding the Agent in `AppState`

`Agent` is **not generic** in 0.42 — the model is erased into a `ModelHandle` at `build()`,
so one concrete `Agent` type covers every provider and `AppState` needs no type parameter.
It is `Clone` and `Send + Sync + 'static`, but cloning copies the whole config (preamble,
static context, hook stack), so `Arc` the agent rather than let axum clone it per request.

This is one **added field**, not a new state type: `db`, `settings`, `clock` and `http` stay
exactly as they are.

```rust
#[derive(Clone)]
pub struct AppState {
    pub db: sea_orm::DatabaseConnection,
    // …the rest of the skeleton's fields, unchanged…
    pub agent: Arc<Agent>,       // new
}
```

Build one agent per role in `main`, beside the database pool, from `settings.agent`
(`build_agent` above), and never inside a handler.

## Mapping `PromptError` into `AppError`

`agent.prompt(..)` fails with `PromptError`, which wraps the provider's `CompletionError`.
The service's `AppError` already has a variant for every outcome a run can have, so
**`error.rs` does not change** — no new variant, no new `status` arm, no `From` impl. One
function in the chat module, `agent_error`, maps a `PromptError` onto what is there, and
the handler calls it with `.map_err(agent_error)?`:

| `PromptError` | `AppError` | Status | Client sees |
|---|---|---|---|
| `MaxTurnsError`, `UnknownToolCall`, `PromptCancelled` | `Other(anyhow::Error::from(error))` | 500 | `"internal server error"` |
| provider answered 429 (`provider_response_status()`) | `TooManyRequests { retry_after_secs }` | 429 | `"too many requests"` + `Retry-After` |
| anything else | `Unavailable(error.to_string())` | 503 | `"service unavailable"` |

The first row is a wiring bug on this side (a budget too small, a tool the model invented),
so it takes the scaffold's *unexpected* path: logged with the whole cause chain, answered
with a constant. The 429 is surfaced as a 429, with the provider's `Retry-After` when it
sends one, so clients back off instead of retrying into the same limit. Everything else is
the dependency this request cannot do without being down, which is exactly what
`Unavailable` is for: the string is **logged, never sent** — a provider message can carry
prompt text, internal URLs and account ids, and `IntoResponse` already renders
`Unavailable` as a constant. The wire shape stays `{ error, request_id, details }`, so
document an agent route with `(status = 503, body = ErrorBody)` and
`(status = 429, body = ErrorBody)` like any other.

`provider_response_status()`, `provider_response_headers()` and `provider_request_id()` are
forwarded through `PromptError` — no destructuring needed.

## The Chat Module

One file, `api/chat.rs`: the request types, the error mapping, a blocking handler, a
streaming handler, the router wiring, and the tests that prove both. It imports `AppError`
and `Valid` from the skeleton; `Valid<T>` gives 422-on-invalid and — the part that matters
on a paid endpoint — rejects an oversized prompt before any model call happens.

```rust,verify,test
//! `api/chat.rs` — the two agent routes and the wiring they need.
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::post;
use futures::{Stream, StreamExt as _};
use rig::agent::{Agent, MultiTurnStreamItem};
use rig::completion::PromptError;
use rig::prelude::*;
use rig::streaming::StreamedAssistantContent;
use serde::{Deserialize, Serialize};
use tower_http::timeout::TimeoutLayer;
use utoipa::ToSchema;
use validator::Validate;

use crate::config::REQUEST_TIMEOUT;
use crate::error::AppError;
use crate::extract::Valid;

/// The skeleton's state with `agent` added; the other fields are unchanged.
#[derive(Clone)]
pub struct AppState {
    pub agent: Arc<Agent>,
}

/// House rules for a request DTO: `deny_unknown_fields`, `ToSchema`, and every
/// `validate` rule repeated as a `schema` constraint.
#[derive(Deserialize, Validate, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatRequest {
    #[validate(length(min = 1, max = 4000))]
    #[schema(min_length = 1, max_length = 4000)]
    pub message: String,
    #[validate(length(min = 1, max = 64))]
    #[schema(min_length = 1, max_length = 64)]
    pub conversation_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ChatResponse {
    pub reply: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Maps a failed run onto the variants `error.rs` already has, so it stays untouched.
fn agent_error(error: PromptError) -> AppError {
    match error {
        // Out of turns, or a tool the model invented: a wiring bug on this side.
        // `Other` is 500, logged with its cause chain, answered with a constant.
        PromptError::MaxTurnsError { .. }
        | PromptError::UnknownToolCall { .. }
        | PromptError::PromptCancelled { .. } => AppError::Other(anyhow::Error::from(error)),
        // A provider 429 is a 429 here too, so clients back off instead of retrying.
        _ if error.provider_response_status() == Some(StatusCode::TOO_MANY_REQUESTS) => {
            let retry_after_secs = error
                .provider_response_headers()
                .and_then(|headers| headers.get(header::RETRY_AFTER))
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok());
            AppError::TooManyRequests { retry_after_secs }
        }
        // The provider is down or broken; the caller's request was fine. The
        // message is logged, never sent: it can carry prompt text, internal URLs
        // and account ids.
        _ => AppError::Unavailable(error.to_string()),
    }
}

/// `skip_all` is deliberate: the default records every argument, which puts the
/// whole prompt in your logs.
#[tracing::instrument(skip_all, fields(conversation_id = body.conversation_id.as_deref()))]
pub async fn chat(
    State(state): State<AppState>,
    Valid(body): Valid<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    // Explicit on every prompt, tools or not: the budget is visible here, and a
    // provider that keeps asking for tools the agent does not have cannot loop.
    let mut request = state.agent.prompt(body.message).max_turns(5);
    if let Some(id) = body.conversation_id {
        request = request.conversation(id);
    }

    // `extended_details()` returns `PromptResponse` (output plus aggregated
    // usage) rather than a bare `String`.
    let response = request.extended_details().await.map_err(agent_error)?;

    Ok(Json(ChatResponse {
        reply: response.output,
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
    }))
}

pub async fn chat_stream(
    State(state): State<AppState>,
    Valid(body): Valid<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    // `stream_prompt` clones the agent's config, so the stream owns its data and
    // is `'static` — it outlives this handler with no borrow of `state`.
    let stream = state.agent.stream_prompt(body.message).max_turns(5).await;

    let events = stream
        .scan(false, |failed, item| {
            // The status line went out with the first token, so a mid-stream
            // failure can only be reported as an event — and then the stream
            // ends, because a client that keeps reading has no way to tell a
            // recovered run from a dead one.
            if *failed {
                return futures::future::ready(None);
            }
            let event = match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
                    text,
                ))) => Some(Event::default().event("token").data(text.text)),
                Ok(MultiTurnStreamItem::FinalResponse(response)) => Some(
                    Event::default()
                        .event("done")
                        .data(response.usage.output_tokens.to_string()),
                ),
                // Tool calls, reasoning deltas, retries: nothing a chat UI draws,
                // so drop them instead of inventing keep-alive comments for them.
                Ok(_) => None,
                Err(error) => {
                    *failed = true;
                    tracing::error!(%error, "chat stream failed");
                    Some(Event::default().event("error").data("the assistant failed"))
                }
            };
            futures::future::ready(Some(event))
        })
        .filter_map(futures::future::ready)
        .map(Ok);

    // Real keep-alive comments, on an interval, from the layer that owns them.
    Ok(Sse::new(events).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// The service's `build_router`, reduced to the agent routes. `/chat/stream` is
/// merged **after** the middleware stack: `TimeoutLayer` would cut a live SSE
/// connection, and `.layer()` wraps only the routes registered before it.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/chat", post(chat))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .merge(Router::new().route("/chat/stream", post(chat_stream)))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use rig::agent::AgentBuilder;
    use rig::completion::Usage;
    use rig::test_utils::{MockCompletionModel, MockStreamEvent, MockTurn};
    use serde_json::json;

    use super::*;

    fn server(model: MockCompletionModel) -> TestServer {
        let agent = AgentBuilder::new(model).preamble("You are terse.").build();
        // axum-test 21 returns the server, not a `Result`.
        TestServer::new(build_router(AppState {
            agent: Arc::new(agent),
        }))
    }

    #[tokio::test]
    async fn a_provider_failure_does_not_leak_the_provider_message() {
        let server = server(MockCompletionModel::from_turns([MockTurn::error(
            "api key sk-live-42 is invalid",
        )]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.text().contains("sk-live-42"), "the body stays generic");
    }

    #[tokio::test]
    async fn a_provider_429_is_answered_with_a_429() {
        let server = server(MockCompletionModel::from_turns([
            MockTurn::provider_response_error(StatusCode::TOO_MANY_REQUESTS, "slow down", "req-1"),
        ]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn the_stream_emits_one_sse_event_per_token() {
        let server = server(MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("Rust "),
            MockStreamEvent::text("is fast."),
            MockStreamEvent::final_response(Usage::default()),
        ]]));

        let response = server
            .post("/chat/stream")
            .json(&json!({ "message": "hi" }))
            .await;

        response.assert_status_ok();
        let body = response.text();
        assert!(body.contains("event: token"), "{body}");
        assert!(body.contains("event: done"), "{body}");
    }

    #[tokio::test]
    async fn a_mid_stream_failure_ends_the_stream() {
        let server = server(MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("Rust "),
            MockStreamEvent::error("api key sk-live-42 is invalid"),
            MockStreamEvent::text("never sent"),
        ]]));

        let response = server
            .post("/chat/stream")
            .json(&json!({ "message": "hi" }))
            .await;

        let body = response.text();
        assert!(body.contains("event: error"), "{body}");
        assert!(!body.contains("never sent"), "the stream stops at the error");
        assert!(!body.contains("sk-live-42"), "the provider message stays in the log");
    }
}
```

## Router Wiring

`build_router` above is the shape, reduced to two routes. In a real service the blocking
route joins the skeleton's `OpenApiRouter` like any other handler — a `#[utoipa::path]`
attribute on `chat` and `.routes(routes!(chat))` — so it appears in the OpenAPI document,
while the streaming route stays on a plain `Router` merged in after the middleware stack:

```rust
let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
    .merge(api::chat::router())        // /chat, documented, inside the stack
    .split_for_parts();

router
    .layer(/* the ServiceBuilder stack, TimeoutLayer innermost */)
    .merge(api::chat::stream_router())  // /chat/stream, outside it
    .with_state(state)
```

Rig does not bound a completion, so the timeout on the blocking route is the only thing
that does. A stream is bounded by the run itself and by the client dropping the connection.

## Notes on the Handlers

- **The stream item type is `Result<MultiTurnStreamItem, StreamingError>` — per chunk, not
  per stream.** Starting a stream always succeeds, so the handler's own error type is
  `Infallible`: the status was committed before the first token and no later failure can
  change it. See Streaming for the full item list.
- **Unhandled items are dropped, not relabelled.** Turning a tool call into
  `Event::comment("keep-alive")` fakes a protocol message the `KeepAlive` layer already
  sends on a real interval; the two collide and a client cannot tell them apart.
- **Cancellation is a drop** — the browser disconnects, axum drops the body, the run ends.
- **Token text is untrusted model output** — escape it client-side.
- **`conversation_id` is recorded, the message is not.** Anything you put in a span field
  is exported; see OpenTelemetry below.

## Testing with `axum-test` and `MockCompletionModel`

`rig::test_utils::{MockCompletionModel, MockTurn, MockStreamEvent, mock_final}` sits behind
the **`test-utils`** feature — `[dev-dependencies]` only, next to `axum-test = "21"`. Because
`Agent` is not generic, a mock-backed agent has the exact type the handlers take, so
`build_router(state)` is the same function in tests and in `main`.

All of these run offline, no API key, no network. `model.clone()` shares the recorded state,
so a clone is your probe — assert `probe.request_count() == 0` on a rejected body to prove
validation ran *before* the paid call.

The inline `mod tests` above builds a router with only the agent in state. In the scaffold the
tests live in `tests/chat.rs` behind `test_app()`, so `tests/common/mod.rs` gains one sibling
that takes the model; everything else in it is unchanged:

```rust
// tests/common/mod.rs delta
use rig::agent::AgentBuilder;
use rig::completion::CompletionModel;
use rig::test_utils::MockCompletionModel;

/// Every test that does not care about the agent gets one that answers one line.
pub async fn test_app() -> TestApp {
    test_app_with_model(MockCompletionModel::text("mock reply")).await
}

pub async fn test_app_with_model(model: impl CompletionModel + 'static) -> TestApp {
    // ...database, migrations, httpmock, `Settings::from_map` exactly as before; add
    // ("APP__AGENT__PROVIDER", "anthropic"), ("APP__AGENT__MODEL", "test") and
    // ("APP__AGENT__API_KEY", "test-key") to the map: `AgentSettings` is required,
    // and unused, because the agent below is built from `model`, not from a client...
    let state = AppState {
        // ...db, settings, clock, http unchanged...
        // Same `Agent` type as production: the model is erased at `build()`.
        agent: Arc::new(AgentBuilder::new(model).name("chat").build()),
    };
    // ...`TestServer::new(build_router(state.clone()))` and the `TestApp` as before...
}
```

A test then reads `let app = common::test_app_with_model(MockCompletionModel::from_turns([
MockTurn::error("api key sk-live-42 is invalid")])).await;` and posts to `app.server` like
any other; `common::test_app()` keeps every existing test green.

## OpenTelemetry

Rig's GenAI spans are plain `tracing` at INFO — **no cargo feature and no env var turns them
on.** The `tracing-opentelemetry` layer from the service skeleton exports them
alongside your axum spans with no extra wiring. Two shapes reach the collector.

| Span | Target | Key fields |
|---|---|---|
| `chat` / `chat_streaming` | `rig::completions` | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.id`, `gen_ai.response.model`, `gen_ai.usage.{input_tokens,output_tokens,cache_read.input_tokens,cache_creation.input_tokens,reasoning_tokens}` |
| `execute_tool` | default | `gen_ai.tool.name`, `gen_ai.tool.call.id`, `gen_ai.tool.call.outcome`, `gen_ai.tool.error.type` |

**The gotcha, specific to a web service:** rig creates the run-level `invoke_agent` span
*only when no enabled span is already current*. Inside an axum handler there always is one
— `OtelAxumLayer`'s server span, or your `#[instrument]` — so rig adopts it and deliberately
skips recording run-level `gen_ai.usage.*` and `gen_ai.completion` onto a span it does not
own. Same agent call: `["invoke_agent", "chat"]` bare, `["http_request", "chat"]` handler.

Per-call usage is still on each `chat` span and `execute_tool` spans still nest under the
request, but the run aggregate is yours to record — read it from `PromptResponse::usage`
after `extended_details()`, as the `chat` handler above does.

- **Set `AgentBuilder::name(..)` on every agent.** Spans are named generically, so
  `gen_ai.agent.name` is the only thing distinguishing two agents in a trace.
- **Keep `record_content_telemetry` `false` in a service.** It puts prompts, retrieved
  context, tool arguments and results onto `gen_ai.input.messages` /
  `gen_ai.output.messages` — user content exported to your backend. Per request only.

Back to the reference index in SKILL.md.
