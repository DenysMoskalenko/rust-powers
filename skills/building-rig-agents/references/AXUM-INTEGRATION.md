# Axum Integration

Read this file when a Rig agent has to live inside an axum service: held in state, called
from a handler, streamed to a browser, tested offline, and traced.

Verified against `rig` 0.45.0, axum 0.8, axum-test 21. The agent adds one sub-struct to
`Settings`, one field to `AppState`, one module, two routes inside the middleware stack and one
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
- [With Conversation Memory](#with-conversation-memory)
- [Testing with `axum-test` and `MockCompletionModel`](#testing-with-axum-test-and-mockcompletionmodel)
- [OpenTelemetry](#opentelemetry)

## Settings

`OpenAI::from_env()` is the quick-start form. In the service the key is a setting like every
other secret: one more sub-struct on `Settings`, read from `APP__AGENT__*` by the same
`config` source the scaffold already has (`Settings::from_map` in tests), `SecretString` so
it never prints, exposed once where the model is connected.

```rust,verify,test
//! `config.rs` delta, and the `build_agent` that `main` calls with it.
use std::collections::HashMap;

use rig::agent::{Agent, AgentBuilder};
use rig::catalog::Catalog;
use rig::providers::registry::ConnectOptions;
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

/// A fixed set is an enum, never a `String`. `IntoStaticStr` gives the vendor id
/// rig's catalog files the provider under, which is `gcp.gemini` for Gemini.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, strum::IntoStaticStr)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    #[strum(serialize = "openai")]
    OpenAi,
    #[strum(serialize = "anthropic")]
    Anthropic,
    #[strum(serialize = "gcp.gemini")]
    Gemini,
}

/// For a Claude id rig's catalog does not list: rig 0.45 fails every prompt to one
/// until `max_tokens` is set, and thinking counts toward it. A listed id keeps rig's
/// default, the model's published output limit; a lower cap cuts replies short.
const UNLISTED_MAX_TOKENS: u64 = 32_000;

/// Built once in `main`, held as `Arc<Agent>` in `AppState`. The catalog connects
/// any `vendor/model` reference, listed or not, to a `DynModel`, so one call
/// covers every provider.
pub fn build_agent(settings: &AgentSettings) -> anyhow::Result<Agent> {
    let vendor: &str = settings.provider.into();
    let reference = format!("{vendor}/{}", settings.model);
    let options = ConnectOptions::new().api_key(settings.api_key.expose_secret());
    let model = Catalog::builtin().connect_with(reference.as_str(), options)?;
    let unlisted_claude = settings.provider == Provider::Anthropic && model.spec().is_none();
    let builder = AgentBuilder::new(model).name("chat").preamble("You are terse.");
    Ok(if unlisted_claude {
        builder.max_tokens(UNLISTED_MAX_TOKENS)
    } else {
        builder
    }
    .build())
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
                .separator("__"),
        )
        .build()
        .unwrap()
        .try_deserialize()
        .unwrap();

    assert_eq!(settings.agent.provider, Provider::Anthropic);
    // Connecting a model is offline; only a prompt talks to the provider.
    assert!(build_agent(&settings.agent).is_ok());
    // The vendor ids are strings now, so check each, with a model the catalog does not list.
    for provider in [Provider::OpenAi, Provider::Anthropic, Provider::Gemini] {
        let unlisted = AgentSettings {
            provider,
            model: "a-model-rig-does-not-list".to_owned(),
            ..settings.agent.clone()
        };
        assert!(build_agent(&unlisted).is_ok(), "{provider:?}");
    }
}
```

## Holding the Agent in `AppState`

`Agent` is **not generic** since 0.42 — `AgentBuilder::new` erases the model, into a `DynModel`
since 0.43, so one concrete `Agent` type covers every provider and `AppState` needs no type parameter.
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

`agent.prompt(..)` fails with `PromptError`; a provider failure arrives as its `Report` variant.
The service's `AppError` already has a variant for every outcome a run can have, so
**`error.rs` does not change** — no new variant, no new `status` arm, no `From` impl. One
function in the chat module, `agent_error`, maps a `PromptError` onto what is there, and
the handler calls it with `.map_err(agent_error)?`. The first row that matches wins:

| `PromptError` | `AppError` | Status | Client sees |
|---|---|---|---|
| `Cancelled` with the reason `DECLINED` (a policy hook's `Stop`) | `BadRequest("the assistant declined this request")` | 400 | that constant |
| `Cancelled` with the reason `UNAVAILABLE` (a policy hook whose own dependency is down) or starting `failed to retrieve dynamic context` (a vector store that is down) | `Unavailable(error.to_string())` | 503 | `"service unavailable"` |
| provider answered 429 (`provider_response_status()`) | `TooManyRequests { retry_after_secs }` | 429 | `"too many requests"` + `Retry-After` |
| a `Report` or `Provider` failure rig marks retryable (`is_retryable()`): no reply at all, a reply cut short, a provider 408, 425 or 5xx, an in-band transient code such as `overloaded_error` | `Unavailable(error.to_string())` | 503 | `"service unavailable"` |
| anything else: `MaxTurns`, `UnknownToolCall`, any other `Cancelled`, `Provider(ProviderError::Response(..))`, a `Report` rig raised itself, any other provider 4xx | `Other(anyhow::Error::from(error))` | 500 | `"internal server error"` |

`DECLINED` and `UNAVAILABLE` are two `const` reasons of this service, defined in the chat module
below. `Memory`, which only an agent built with `.memory(..)` raises, has its own handling
under With Conversation Memory.

A policy hook declining this request is a normal outcome rather than a fault, so it is a 400.
Every such hook stops with the one constant, `DECLINED`, because rig raises `Cancelled`
for its own failures too (a lost prompt, a driver protocol violation, a tool round with no
results), and those are faults. A dependency that is down stays a 503 even when it ends the run
through `Cancelled`: a hook whose own check failed stops with `UNAVAILABLE`, and rig's
`dynamic_context` says only in its message that the vector store failed. The 429 is surfaced as
a 429, with the provider's `Retry-After` when it sends one, so clients back off instead of
retrying into the same limit; it is matched before the retryable row, because rig marks a 429
retryable too.

The 503 row does not keep a list of its own: rig classifies every provider failure once, and
`is_retryable()` is that verdict, read from the `Report` or from the `ProviderError`. It is true
exactly when the same request may succeed later, which is what `Unavailable` says to a client.
Everything rig does not mark retryable fails the same way again (a budget too small, a tool the
model invented, a run rig cancelled itself, a request rig could not build, a hook that refused
the completion, a revoked key, an unknown model id, a request shape the provider rejects), so
it takes the scaffold's *unexpected* path, `Other`. A 503 there would read as an outage and
invite clients to retry what can never succeed.

`AppError` logs every 5xx once, at ERROR with the whole cause chain, and answers with a
constant, so neither `agent_error` nor the handler logs the failure again. That log line is the
only place the provider's message goes: the string in `Unavailable` and the error in `Other`
are **logged, never sent**, because a provider message can carry prompt text, internal URLs
and account ids. The wire shape stays `{ error, request_id, details }`, so document an agent
route with `(status = 503, body = ErrorBody, description = "The model provider is unavailable")`
and `(status = 429, body = ErrorBody, description = "The model provider's rate limit")` like any
other.

Since rig 0.44 a turn the provider ended in a content filter or an unknown finish reason, and
malformed tool arguments that ended the run, fail as `Provider(ProviderError::Response(..))`
with no status. rig marks that variant not retryable, so it is a 500. A turn that ends at the
output-token limit is different: rig returns its text as a success, so the handler checks the
last call's `finish_reason` and logs a truncated reply at WARN.

`provider_response_status()`, `provider_response_headers()` and `provider_request_id()` are
forwarded through `PromptError`, from its `Report` as well — no destructuring needed.

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
use rig::agent::{Agent, MultiTurnStreamItem, PromptResponse};
use rig::completion::{FinishReason, PromptError};
use rig::streaming::{Item, StreamEvent};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use validator::Validate;

use crate::api::request_timeout;
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
}

/// A count is `None` when the provider did not report it.
#[derive(Serialize, ToSchema)]
pub struct ChatResponse {
    pub reply: String,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// The reason every policy hook in this service stops a run with, as in
/// `CompletionCallAction::stop(DECLINED)`. rig raises `Cancelled` for its
/// own failures too, so the reason is how `agent_error` tells a refusal from a fault.
/// A hook that could not decide, say because its moderation call failed, stops
/// with `UNAVAILABLE` instead: that is an outage, not a refusal.
pub const DECLINED: &str = "declined by policy";

/// The reason a policy hook stops with when its own dependency is down.
pub const UNAVAILABLE: &str = "policy check unavailable";

/// The longest gap between two items of a streamed run. No item arrives while a
/// tool runs, so it has to exceed the slowest tool's own timeout; `REQUEST_TIMEOUT`
/// budgets a whole unary request and is too short for that.
pub const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Maps a failed run onto the variants `error.rs` already has, so it stays untouched.
/// The first arm that matches wins.
fn agent_error(error: PromptError) -> AppError {
    match error {
        // A policy hook said no: a refusal, not a fault, answered with a constant.
        PromptError::Cancelled { reason, .. } if reason == DECLINED => {
            AppError::BadRequest("the assistant declined this request".to_owned())
        }
        // A dependency of the run is down, which is a 503 like any backend: a hook
        // whose own check failed, or a vector store `dynamic_context` could not
        // query. rig marks the latter only in its message.
        PromptError::Cancelled { ref reason, .. }
            if reason == UNAVAILABLE
                || reason.starts_with("failed to retrieve dynamic context") =>
        {
            AppError::Unavailable(error.to_string())
        }
        // A provider 429 is a 429 here too, so clients back off instead of retrying.
        // Before the next arm: rig marks a 429 retryable as well.
        _ if error.provider_response_status() == Some(StatusCode::TOO_MANY_REQUESTS) => {
            let retry_after_secs = error
                .provider_response_headers()
                .and_then(|headers| headers.get(header::RETRY_AFTER))
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok());
            AppError::TooManyRequests { retry_after_secs }
        }
        // The provider is down or broken and the same request may succeed later. The
        // message is logged, never sent: it can carry prompt text, internal URLs
        // and account ids.
        _ if is_retryable(&error) => AppError::Unavailable(error.to_string()),
        // Everything else fails the same way again, so a 503 would invite clients
        // to retry what can never succeed: out of turns, a tool the model invented,
        // a run rig cancelled itself, a filtered or failed turn, a request rig could
        // not build, a hook that refused the completion, a revoked key, an unknown
        // model id. `Other` is 500, logged with its cause chain, answered with a
        // constant.
        _ => AppError::Other(anyhow::Error::from(error)),
    }
}

/// rig's own verdict that the same request may succeed later: no reply at all, a
/// reply cut short, a provider 408, 425, 429 or 5xx, or an in-band error envelope
/// with a transient code. rig sets it once, when it classifies the failure.
fn is_retryable(error: &PromptError) -> bool {
    match error {
        PromptError::Report(report) => report.is_retryable(),
        PromptError::Provider(provider) => provider.is_retryable(),
        _ => false,
    }
}

/// rig returns a reply the output-token limit cut short as a success, so this line
/// is the only sign that a 200 is incomplete. The fix is a larger `max_tokens`.
fn warn_if_truncated(response: &PromptResponse) {
    let last = response.completion_calls.last();
    if matches!(
        last.and_then(|call| call.finish_reason.as_ref()),
        Some(FinishReason::Length)
    ) {
        tracing::warn!("the reply hit the output-token limit and is cut short");
    }
}

/// `skip_all` is deliberate: the default records every argument, which puts the
/// whole prompt in your logs.
#[tracing::instrument(skip_all)]
pub async fn chat(
    State(state): State<AppState>,
    Valid(body): Valid<ChatRequest>,
) -> Result<Json<ChatResponse>, AppError> {
    // Explicit on every prompt, tools or not: the budget is visible here, and a
    // provider that keeps asking for tools the agent does not have cannot loop.
    let request = state.agent.prompt(body.message).max_turns(5);

    // A `PromptResponse`: the output plus usage aggregated over the run.
    let response = request.await.map_err(agent_error)?;
    warn_if_truncated(&response);

    Ok(Json(ChatResponse {
        reply: response.output(),
        input_tokens: response.usage.input_tokens,
        output_tokens: response.usage.output_tokens,
    }))
}

pub async fn chat_stream(
    State(state): State<AppState>,
    Valid(body): Valid<ChatRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    // The runner clones the agent's config, so the stream owns its data and is
    // `'static` — it outlives this handler with no borrow of `state`.
    let run = state.agent.prompt(body.message).max_turns(5).stream();

    // Real keep-alive comments, on an interval, from the layer that owns them.
    Ok(Sse::new(sse_events(run)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// The run as SSE events. Rig does not bound a completion and the request timeout
/// stops at the response head, so every item must arrive within
/// `STREAM_IDLE_TIMEOUT`. `Timeout` yields one `Elapsed` and then waits on the run
/// again, so the state turns `None` after the last frame: the next poll ends the
/// stream without touching the run, which drops it and the provider call.
fn sse_events(
    run: impl Stream<Item = Result<MultiTurnStreamItem, PromptError>> + 'static,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let run = Box::pin(tokio_stream::StreamExt::timeout(run, STREAM_IDLE_TIMEOUT));
    futures::stream::unfold(Some(run), |run| async move {
        let mut run = run?;
        loop {
            match frame(run.next().await?) {
                Frame::Skip => {}
                Frame::Send(event) => return Some((Ok::<_, Infallible>(event), Some(run))),
                Frame::Last(event) => return Some((Ok(event), None)),
            }
        }
    })
}

/// What one item of the run becomes on the wire.
enum Frame {
    /// Tool calls, reasoning deltas, retries: nothing a chat UI draws. Dropped
    /// rather than relabelled as keep-alive comments, which `KeepAlive` owns.
    Skip,
    Send(Event),
    /// The status line went out with the first token, so a failure can only be
    /// an event, and it is the last one: a client that keeps reading cannot
    /// tell a recovered run from a dead one.
    Last(Event),
}

fn frame(
    item: Result<Result<MultiTurnStreamItem, PromptError>, tokio_stream::Elapsed>,
) -> Frame {
    let cause = match item {
        Ok(Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
            text,
            ..
        })))) => {
            return Frame::Send(Event::default().event("token").data(text));
        }
        Ok(Ok(MultiTurnStreamItem::FinalResponse(response))) => {
            warn_if_truncated(&response);
            // Empty when the provider reported no count.
            let output_tokens = response
                .usage
                .output_tokens
                .map_or_else(String::new, |tokens| tokens.to_string());
            return Frame::Send(Event::default().event("done").data(output_tokens));
        }
        Ok(Ok(_)) => return Frame::Skip,
        Ok(Err(PromptError::Cancelled { reason, .. })) if reason == DECLINED => {
            return Frame::Last(
                Event::default()
                    .event("declined")
                    .data("the assistant declined this request"),
            );
        }
        Ok(Err(error)) => error.to_string(),
        Err(stalled) => stalled.to_string(),
    };
    tracing::error!(%cause, "chat stream failed");
    Frame::Last(Event::default().event("error").data("the assistant failed"))
}

/// The service's `build_router`, reduced to the agent routes and the one layer of
/// the stack that decides how long they may run. The scaffold's `request_timeout`
/// races only the future that produces the response, so it bounds `/chat`, with a
/// 503 in `ErrorBody`, and never cuts a stream that has started.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/chat", post(chat))
        .route("/chat/stream", post(chat_stream))
        .layer(axum::middleware::from_fn(request_timeout))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use axum_test::TestServer;
    use futures::StreamExt as _;
    use rig::agent::{
        AgentBuilder, AgentHook, CompletionCallAction, CompletionCallEvent, HookContext,
    };
    use rig::completion::Usage;
    use rig::test_utils::{MockCompletionModel, MockStreamEvent, MockTurn};
    use serde_json::json;

    use super::*;
    use crate::config::REQUEST_TIMEOUT;

    fn server(model: MockCompletionModel) -> TestServer {
        let agent = AgentBuilder::new(model).preamble("You are terse.").build();
        // axum-test 21 returns the server, not a `Result`.
        TestServer::new(build_router(AppState {
            agent: Arc::new(agent),
        }))
    }

    #[tokio::test]
    async fn a_provider_outage_is_a_503_without_the_provider_message() {
        // 408, 425 and every 5xx are the statuses rig marks retryable besides 429.
        for status in [
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_EARLY,
        ] {
            let server = server(MockCompletionModel::from_turns([
                MockTurn::provider_response_error(
                    status,
                    "upstream overloaded for account acct-42",
                    "req-0",
                ),
            ]));

            let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

            response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
            assert!(!response.text().contains("acct-42"), "the body stays generic");
        }
    }

    #[tokio::test]
    async fn a_failure_rig_does_not_mark_retryable_is_a_500() {
        // `MockTurn::error` is `ProviderError::Provider`, which rig never marks retryable.
        let server = server(MockCompletionModel::from_turns([MockTurn::error(
            "unexpected reply for account acct-42",
        )]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!response.text().contains("acct-42"), "the body stays generic");
    }

    #[tokio::test]
    async fn a_rejected_api_key_is_a_500_not_an_outage() {
        let server = server(MockCompletionModel::from_turns([
            MockTurn::provider_response_error(
                StatusCode::UNAUTHORIZED,
                "api key sk-live-42 is invalid",
                "req-2",
            ),
        ]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!response.text().contains("sk-live-42"), "the body stays generic");
    }

    #[tokio::test]
    async fn an_unknown_model_id_is_a_500_not_an_outage() {
        let server = server(MockCompletionModel::from_turns([
            MockTurn::provider_response_error(StatusCode::NOT_FOUND, "model not found", "req-3"),
        ]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn a_request_rig_cannot_build_is_a_500_not_an_outage() {
        let server = server(MockCompletionModel::from_turns([MockTurn::request_error(
            "`max_tokens` must be set for Anthropic",
        )]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn a_filtered_turn_is_a_500_not_an_outage() {
        let server = server(MockCompletionModel::from_turns([
            MockTurn::text("partial").with_finish_reason(FinishReason::ContentFilter),
        ]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn a_truncated_reply_is_still_a_200() {
        let server = server(MockCompletionModel::from_turns([
            MockTurn::text("partial").with_finish_reason(FinishReason::Length),
        ]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status_ok();
        assert_eq!(response.json::<serde_json::Value>()["reply"], "partial");
    }

    #[tokio::test]
    async fn a_provider_429_is_answered_with_a_429() {
        let server = server(MockCompletionModel::from_turns([
            MockTurn::provider_response_error(StatusCode::TOO_MANY_REQUESTS, "slow down", "req-1"),
        ]));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::TOO_MANY_REQUESTS);
    }

    /// A policy hook that declines every request before the model is called.
    struct DeclineAll;

    impl AgentHook for DeclineAll {
        async fn on_completion_call(
            &self,
            _ctx: &HookContext,
            _event: CompletionCallEvent<'_>,
        ) -> CompletionCallAction {
            CompletionCallAction::stop(DECLINED)
        }
    }

    #[tokio::test]
    async fn a_policy_stop_is_a_400_with_a_constant_body() {
        let agent = AgentBuilder::new(MockCompletionModel::text("never sent"))
            .add_hook(DeclineAll)
            .build();
        let server = TestServer::new(build_router(AppState {
            agent: Arc::new(agent),
        }));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::BAD_REQUEST);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["error"], "the assistant declined this request");
    }

    /// A policy hook whose moderation backend is down.
    struct ModerationDown;

    impl AgentHook for ModerationDown {
        async fn on_completion_call(
            &self,
            _ctx: &HookContext,
            _event: CompletionCallEvent<'_>,
        ) -> CompletionCallAction {
            CompletionCallAction::stop(UNAVAILABLE)
        }
    }

    #[tokio::test]
    async fn a_policy_check_outage_is_a_503() {
        let agent = AgentBuilder::new(MockCompletionModel::text("never sent"))
            .add_hook(ModerationDown)
            .build();
        let server = TestServer::new(build_router(AppState {
            agent: Arc::new(agent),
        }));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn a_policy_stop_on_the_stream_is_a_declined_event() {
        let agent = AgentBuilder::new(MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("never sent"),
        ]]))
        .add_hook(DeclineAll)
        .build();
        let server = TestServer::new(build_router(AppState {
            agent: Arc::new(agent),
        }));

        let response = server
            .post("/chat/stream")
            .json(&json!({ "message": "hi" }))
            .await;

        let body = response.text();
        assert!(body.contains("event: declined"), "{body}");
        assert!(!body.contains("never sent"), "{body}");
    }

    /// A model call that outlasts the request budget.
    struct Slow;

    impl AgentHook for Slow {
        async fn on_completion_call(
            &self,
            _ctx: &HookContext,
            _event: CompletionCallEvent<'_>,
        ) -> CompletionCallAction {
            tokio::time::sleep(REQUEST_TIMEOUT + Duration::from_secs(1)).await;
            CompletionCallAction::continue_run()
        }
    }

    // Paused time is safe here: this router has no database, so nothing waits on
    // real I/O while the clock jumps.
    #[tokio::test(start_paused = true)]
    async fn a_run_past_the_request_budget_is_a_503() {
        let agent = AgentBuilder::new(MockCompletionModel::text("too late"))
            .add_hook(Slow)
            .build();
        let server = TestServer::new(build_router(AppState {
            agent: Arc::new(agent),
        }));

        let response = server.post("/chat").json(&json!({ "message": "hi" })).await;

        response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
        let body = response.json::<serde_json::Value>();
        assert_eq!(body["error"], "service unavailable");
    }

    #[tokio::test(start_paused = true)]
    async fn the_request_budget_does_not_cut_a_stream() {
        let agent = AgentBuilder::new(MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("slow but streamed"),
            MockStreamEvent::final_response(Usage::default()),
        ]]))
        .add_hook(Slow)
        .build();
        let server = TestServer::new(build_router(AppState {
            agent: Arc::new(agent),
        }));

        let response = server
            .post("/chat/stream")
            .json(&json!({ "message": "hi" }))
            .await;

        let body = response.text();
        assert!(body.contains("event: token"), "{body}");
        assert!(body.contains("event: done"), "{body}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_run_ends_the_stream_after_one_error_event() {
        let stalled = futures::stream::pending::<Result<MultiTurnStreamItem, PromptError>>();

        let events: Vec<_> = sse_events(stalled).collect().await;

        assert_eq!(events.len(), 1, "one error event, then the stream ends");
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

`build_router` above is the shape, reduced to two routes. In a real service both join the
skeleton's `OpenApiRouter` like any other handler — a `#[utoipa::path]` attribute on each and
`.routes(routes!(..))` — so they appear in the OpenAPI document and sit inside the middleware
stack with every other route: the server span, request id, panic handling, metrics, CORS,
compression, the body limit and, innermost, the request timeout.

```rust
// api/chat.rs, merged in `api::router()` beside `users::router()`
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(chat))
        .routes(routes!(chat_stream))
}
```

Rig does not bound a completion. On `/chat` the scaffold's `api::request_timeout` middleware
does: past `REQUEST_TIMEOUT` it drops the run, and the provider call with it, and answers
`AppError::Unavailable`, a 503 in `ErrorBody`. A run that cannot fit that budget, with its
tool calls and its thinking, belongs on the stream route. On `/chat/stream` the middleware
stops at the response head, which leaves before the first token, so `sse_events` wraps the run
in `tokio_stream::StreamExt::timeout` with a constant of its own, `STREAM_IDLE_TIMEOUT`: no item
arrives while a tool runs, so the gap it allows has to exceed the slowest tool's timeout, which
`REQUEST_TIMEOUT` does not. The timeout alone does not end anything: it yields one `Elapsed` and
then waits on the run again, while the SSE keep-alive, which the timeout never sees, stops any
proxy from closing the idle connection. So after the `error` event the `unfold` state is `None`,
and the next poll ends the stream without touching the run, which drops it and the provider call
with it. `max_turns` bounds how many model calls one run makes.

## Notes on the Handlers

- **The stream item type is `Result<MultiTurnStreamItem, PromptError>` — per chunk, not
  per stream.** Starting a stream always succeeds, so the handler's own error type is
  `Infallible`: the status was committed before the first token and no later failure can
  change it. See Streaming for the full item list.
- **Unhandled items are dropped, not relabelled.** Turning a tool call into
  `Event::comment("keep-alive")` fakes a protocol message the `KeepAlive` layer already
  sends on a real interval; the two collide and a client cannot tell them apart.
- **Cancellation is a drop** — the browser disconnects, axum drops the body, the run ends.
- **Token text is untrusted model output** — escape it client-side.
- **A conversation id is scoped to the caller.** The agent above has no `.memory(..)`, so each
  request stands alone. With memory, `ChatRequest` gains a `conversation_id: Option<Uuid>`, which
  is untrusted input: key the log by the authenticated user as well, or one user's guessed id
  reads another user's history. With axum-service's `AuthUser` as a handler argument before the
  body extractor:

  ```rust
  let request = state.agent.prompt(body.message).max_turns(5);
  let request = match body.conversation_id {
      // The client names the conversation; the token decides whose it is. A log
      // name is one path segment, so `/` and `\` in a subject are escaped, `%` first
      // so that no two subjects map to one name.
      Some(id) => {
          let owner = user.0.sub.replace('%', "%25").replace('/', "%2F").replace('\\', "%5C");
          request.conversation(format!("{owner}-{id}"))
      }
      // With no conversation the run would save to the one log named after
      // the agent, which every caller shares.
      None => request.without_memory(),
  };
  ```

  A run of such an agent can also fail in its memory store; see With Conversation Memory.

## With Conversation Memory

An agent built with `.memory(..)` adds one failure, `PromptError::Memory { error, response }`,
and it means two different things. With `response: None` the conversation could not be loaded,
before any model call. With `response: Some(..)` the run finished and only its save failed:
the tools ran and the tokens are spent, so the handler returns that answer and logs the lost
save; answering 503 would have the client retry and run the tools a second time. The handler
takes `Memory` out before `agent_error` sees the rest. `rig::memory::SessionError` needs `rig`'s
`memory` feature.

```rust,verify,test
//! `api/chat.rs` additions for an agent built with `.memory(..)`.
use std::io;

use rig::agent::PromptResponse;
use rig::memory::SessionError;

use crate::error::AppError;

/// In `chat`, in place of `request.await.map_err(agent_error)?`:
///
/// ```text
/// let response = match request.await {
///     Err(PromptError::Memory { error, response }) => memory_failure(error, response)?,
///     other => other.map_err(agent_error)?,
/// };
/// ```
fn memory_failure(
    error: SessionError,
    response: Option<Box<PromptResponse>>,
) -> Result<PromptResponse, AppError> {
    match response {
        // The run finished and only its save failed, so its answer is returned.
        // The turn is missing from the log, which only this line says.
        Some(response) => {
            tracing::error!(%error, "the conversation was not saved; returning the finished answer");
            Ok(*response)
        }
        None => Err(memory_error(error)),
    }
}

/// A memory failure that left no answer, on the variants `error.rs` already has.
fn memory_error(error: SessionError) -> AppError {
    match error {
        // Another run saved to this conversation first. An agent run raises it on
        // the save, so with an answer; a `save_async` of your own hands it here.
        SessionError::Changed { .. } => {
            AppError::Conflict("the conversation changed while this request ran".to_owned())
        }
        // The store is down. `InvalidInput` is not an outage: it is a log name rig
        // refused, a bug in the code that built the name.
        SessionError::Io(ref failure) if failure.kind() != io::ErrorKind::InvalidInput => {
            AppError::Unavailable(error.to_string())
        }
        // A refused name, or a session another rig version wrote, fails the same
        // way on every retry.
        _ => AppError::Other(anyhow::Error::from(error)),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse as _;
    use rig::agent::{
        AgentBuilder, AgentHook, CompletionCallAction, CompletionCallEvent, HookContext,
    };
    use rig::completion::{Message, PromptError};
    use rig::memory::{MemoryStore, load, save};
    use rig::test_utils::MockCompletionModel;

    use super::*;

    /// Stands in for a second request on the same conversation: it saves a turn
    /// after this run loaded the log and before this run saves.
    struct OtherRequestSavesFirst(MemoryStore);

    impl AgentHook for OtherRequestSavesFirst {
        async fn on_completion_call(
            &self,
            _ctx: &HookContext,
            _event: CompletionCallEvent<'_>,
        ) -> CompletionCallAction {
            let mut other = load(&self.0, "u-1").unwrap();
            other.push(Message::user("from the other request"));
            save(&self.0, "u-1", &mut other).unwrap();
            CompletionCallAction::continue_run()
        }
    }

    #[tokio::test]
    async fn a_run_whose_save_failed_still_answers() {
        let store = MemoryStore::default();
        let agent = AgentBuilder::new(MockCompletionModel::text("the answer"))
            .memory(store.clone())
            .add_hook(OtherRequestSavesFirst(store))
            .build();

        let failed = agent.prompt("hi").conversation("u-1").max_turns(1).await;

        let Err(PromptError::Memory { error, response }) = failed else {
            panic!("the later of two saves fails");
        };
        assert!(matches!(error, SessionError::Changed { .. }));
        let answer = memory_failure(error, response).map(|response| response.output());
        assert_eq!(answer.ok().as_deref(), Some("the answer"));
    }

    #[tokio::test]
    async fn a_failed_load_maps_onto_the_existing_variants() {
        let agent = AgentBuilder::new(MockCompletionModel::text("never sent"))
            .memory(MemoryStore::default())
            .build();

        // `/` is not allowed in a log name, so rig refuses the load.
        let refused = agent.prompt("hi").conversation("a/b").max_turns(1).await;

        let Err(PromptError::Memory { error, response }) = refused else {
            panic!("a refused log name fails the load");
        };
        let status = |error, response| match memory_failure(error, response) {
            Ok(_) => StatusCode::OK,
            Err(error) => error.into_response().status(),
        };
        assert_eq!(status(error, response), StatusCode::INTERNAL_SERVER_ERROR);

        let down = io::Error::new(io::ErrorKind::ConnectionRefused, "store down");
        let down = SessionError::Io(down);
        assert_eq!(status(down, None), StatusCode::SERVICE_UNAVAILABLE);

        let changed = SessionError::Changed {
            log: "u-1".to_owned(),
        };
        assert_eq!(status(changed, None), StatusCode::CONFLICT);
    }
}
```

A streamed run fails the same way: the stream yields every token and then, in place of
`FinalResponse`, the `Memory` error carrying the response. In `frame` that item is the answer's
normal end, not a failure, so match it before the generic error arm, log it as `memory_failure`
does and send the `done` event built from its `response`.

Returning the answer does not put the turn back: the next request of that conversation loads a
log without it. Two requests on one conversation that overlap always end that way for the later
one, so keep them from overlapping: take a lease on the log name before the run (rust-redis
owns the lease) and answer the request that finds it held with `AppError::Conflict`.

## Testing with `axum-test` and `MockCompletionModel`

`rig::test_utils::{MockCompletionModel, MockTurn, MockStreamEvent, mock_final}` sits behind
the **`test-utils`** feature — `[dev-dependencies]` only, next to `axum-test = "21"`. Because
`Agent` is not generic, a mock-backed agent has the exact type the handlers take, so
`build_router(state)` is the same function in tests and in `main`.

All of these run offline, no API key, no network. `model.clone()` shares the recorded state,
so a clone is your probe — assert `probe.request_count() == 0` on a rejected body to prove
validation ran *before* the paid call.

The inline `mod tests` above builds a router with only the agent in state. In the scaffold the
tests live in `tests/api/chat.rs`, declared with `mod chat;` in `tests/api/main.rs`, behind
`test_app()`, so `tests/api/common.rs` gains one sibling that takes the model; everything else in
it is unchanged:

```rust
// tests/api/common.rs delta
use rig::DynModel;
use rig::agent::AgentBuilder;
use rig::operation::Completion;
use rig::test_utils::MockCompletionModel;

/// Every test that does not care about the agent gets one that answers one line.
pub async fn test_app() -> TestApp {
    test_app_with_model(MockCompletionModel::text("mock reply")).await
}

pub async fn test_app_with_model(model: impl Into<DynModel<Completion>>) -> TestApp {
    // ...database, migrations, httpmock, `Settings::from_map` exactly as before; add
    // ("APP__AGENT__PROVIDER", "anthropic"), ("APP__AGENT__MODEL", "test") and
    // ("APP__AGENT__API_KEY", "test-key") to the map: `AgentSettings` is required,
    // and unused, because the agent below is built from `model`, not from a client...
    let state = AppState {
        // ...db, settings, clock, http unchanged...
        // Same `Agent` type as production: the model is erased at `AgentBuilder::new`.
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
| `chat` (`gen_ai.request.stream` is `true` when streamed) | `rig::agent_chat` (`rig::completions` for a direct model call) | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.id`, `gen_ai.response.model`, `gen_ai.usage.{input_tokens,output_tokens,cache_read.input_tokens,cache_creation.input_tokens,reasoning_tokens}` |
| `execute_tool` | default | `gen_ai.tool.name`, `gen_ai.tool.call.id`, `gen_ai.tool.call.outcome`, `gen_ai.tool.error.type` |

**The gotcha, specific to a web service:** rig creates the run-level `invoke_agent` span
*only when no enabled span is already current*. Inside an axum handler there always is one
— the scaffold's `request` span, a child of `OtelAxumLayer`'s server span, or your
`#[instrument]` — so rig adopts it and deliberately skips recording run-level `gen_ai.usage.*`
and `gen_ai.completion` onto a span it does not own. Same agent call: `["invoke_agent", "chat"]`
bare; in a handler the `chat` spans sit directly under the span rig adopted.

Per-call usage is still on each `chat` span and `execute_tool` spans still nest under the
request, but the run aggregate is yours to record — read it from `PromptResponse::usage`,
as the `chat` handler above does.

- **Set `AgentBuilder::name(..)` on every agent.** Spans are named generically, so
  `gen_ai.agent.name` is the only thing distinguishing two agents in a trace.
- **Keep `record_content_telemetry` `false` in a service.** It puts prompts, retrieved
  context, tool arguments and results onto `gen_ai.input.messages` /
  `gen_ai.output.messages` — user content exported to your backend. Per request only.

Back to the reference index in SKILL.md.
