# Errors and Reliability

Read this file when a Rig call fails, a build fails, or the user needs retries, rate-limit
handling, or production hardening.

Verified against `rig` 0.43.0.

## Contents

- [Compile Errors First](#compile-errors-first)
- [`PromptError`](#prompterror)
- [`ProviderError`](#providererror)
- [Rig Does Not Retry](#rig-does-not-retry)
- [Runtime Failures That Compile Fine](#runtime-failures-that-compile-fine)
- [Production Checklist](#production-checklist)

## Compile Errors First

Most compile errors against rig 0.43 come from code written for 0.42. Check the shape
before you doubt the imports.

### `cannot find 'Client' in 'openai'`, `no method named 'agent' / 'embedding_model' found`

```text
error[E0433]: cannot find `Client` in `openai`
```

That is 0.42 code. rig 0.43 has no client traits: a provider client is a concrete type
(`OpenAI`, `Anthropic`, `Gemini`) whose methods are inherent, and an agent wraps one of its
models — `AgentBuilder::new(OpenAI::from_env()?.completion(MODEL))`. `prompt`, `chat` and
`prompt_typed` are inherent on `Agent`, so no trait import is missing either;
`use rig::prelude::*;` re-exports types such as `AgentBuilder` and `Agent`, plus the `Tool`,
`PortableTool` and `VectorStoreIndex` traits (`top_n` needs the last in scope). The
0.42 → 0.43 table in Version Drift pairs every old call with its replacement.

### `the async keyword is missing from the function declaration`

`#[tokio::main]` needs Tokio's macro and runtime features:

```bash
cargo add tokio --features macros,rt-multi-thread
```

### `unresolved import rig::...`

Capabilities are feature-gated. The `agent` feature (on by default) carries `Agent`,
hooks, extractors and the tool registry; the `Tool` trait itself needs no feature. The rest are opt-in: `test-utils` (mocks, dev-dependencies
only), `rmcp` (MCP tools), `memory` (history-bounding policies), `pdf` and `epub` (file
loaders), `audio`, `image`, and one per vector store (`lancedb`, `qdrant`, `postgres`, …).

### `Agent<M>` does not compile

`Agent` has no type parameter in 0.43 — `AgentBuilder::new` takes the model as a
`DynModel`. Write `Agent`. Same for hooks: `impl AgentHook for MyHook`, not
`impl<M: CompletionModel> AgentHook<M> for MyHook`. See
Version Drift.

## `PromptError`

`agent.prompt(..)` returns `Result<PromptResponse, PromptError>`:

```rust
pub enum PromptError {
    CompletionError(ProviderError),
    Report(ErrorReport),           // a failure relayed over the agent's bus
    MemoryError(MemoryError),
    MaxTurnsError {
        max_turns: usize,
        chat_history: Vec<Message>,
        prompt: Message,
    },
    PromptCancelled {
        chat_history: Vec<Message>,
        reason: String,
    },
    UnknownToolCall {
        tool_name: String,
        available_tools: Vec<String>,
        allowed_tools: Vec<String>,
        chat_history: Vec<Message>,
    },
}
```

A provider failure usually reaches an agent run as `Report`, not `CompletionError`: the provider's
reply crosses the agent's bus as an `ErrorReport`, whose `kind` classifies it and whose
`provider_response_status()` still reads the HTTP status.

```rust
use rig::completion::PromptError;

match agent.prompt("What is 2 + 2?").max_turns(3).await {
    Ok(reply) => println!("{}", reply.output),

    Err(PromptError::MaxTurnsError { max_turns, chat_history, .. }) => {
        // The model kept calling tools past the budget.
        eprintln!("hit the {max_turns}-call budget after {} messages", chat_history.len());
    }

    Err(PromptError::UnknownToolCall { tool_name, available_tools, .. }) => {
        // The model invented a tool. Recoverable with a hook.
        eprintln!("model called {tool_name}; available: {available_tools:?}");
    }

    Err(PromptError::PromptCancelled { reason, .. }) => {
        // A hook returned a stop action, or rig cancelled the run itself (a lost
        // prompt, a protocol violation): only the reason tells them apart.
        eprintln!("cancelled: {reason}");
    }

    Err(PromptError::MemoryError(e)) => eprintln!("memory backend failed: {e}"),
    Err(PromptError::Report(report)) => eprintln!("{:?} failure: {report}", report.kind),
    Err(PromptError::CompletionError(e)) => eprintln!("provider call failed: {e}"),
}
```

Every variant that can carries `chat_history`, so a failure is diagnosable without extra
instrumentation. Log it (minus content you should not persist) when a run fails in
production.

**`MaxTurnsError` is usually a design signal, not a transient fault.** Either the budget is
too low for the tool chain (raise `max_turns`), or the model is stuck in a loop because the
tools are too many, too similar, or badly described. Retrying the same prompt with the same
budget will fail the same way.

**`UnknownToolCall`** is fail-fast by default. To recover — retry with corrective feedback,
repair the name, or skip — implement `on_invalid_tool_call`; see
Hooks and Runner.

## `ProviderError`

Raw model calls return `ProviderError`, one type for every operation, which separates
transport failures from provider-reported ones:

```rust
pub enum ProviderError {
    Http(Arc<http_client::Error>),             // no reply: connection, timeout
    Json(Arc<serde_json::Error>),              // (de)serialization
    Url(ParseError),
    Request(SharedError),                      // the request could not be built
    Response(String),                          // the reply decoded but does not answer the request
    Provider(String),
    ProviderResponse(ProviderResponseError),   // the provider's reply, status and body
    InvalidAuthentication(ProviderResponseError), // `verify()` found the key rejected
    Truncated,                                 // the reply stopped before the provider ended it
    // ...and a few narrower variants
}
```

Rough triage:

| Variant | Usually | Action |
|---|---|---|
| `Http`, `Truncated` | Network or timeout, no usable reply | Retry with backoff |
| `ProviderResponse` | **Every non-2xx reply**, 401 and 429 alike | Check the status before retrying |
| `InvalidAuthentication` | `verify()` found the key rejected | Fix the key; never retry |
| `Provider` | The provider reported a failure without a preserved reply | Surface it; retrying rarely helps |
| `Response` | The reply decoded but does not answer the request | Surface it; retrying rarely helps |
| `Json`, `Url`, `Request` | Your bug, or for `Json` a reply that does not decode | Fix the code |

`ProviderResponse` is not a synonym for "transient". Rig puts every non-2xx reply into it,
so a 401 (bad key), a 403, and a 400 (malformed request) land in the same variant as a 429
or a 503. Branch on `provider_response_status()` before retrying — retry 408, 429, and
5xx; surface 4xx immediately. A blind retry on `ProviderResponse` turns a wrong API key into
three wrong API keys.

### Inspecting a provider failure

`ProviderError` exposes the provider's raw HTTP status and body so you can branch on a
specific error code rather than string-matching a message:

```rust,verify
use rig::error::ProviderError;
fn report(error: &ProviderError) {
    if let Some(status) = error.provider_response_status() {
        // Can be a 2xx for providers that return an error envelope with a success status.
        eprintln!("provider returned HTTP {status}");
    }
    match error.provider_response_json() {
        Ok(Some(json)) => eprintln!("provider error payload: {json}"),
        Ok(None) => eprintln!("no provider response body (transport error)"),
        Err(_) => eprintln!("body was not JSON: {:?}", error.provider_response_body()),
    }
}
```

Embedding, image, audio, transcription and rerank calls fail with the same `ProviderError`,
and `PromptError` forwards the same accessors, from its `Report` as well.

Do not log the raw body indiscriminately — it can echo your prompt back.

## Rig Does Not Retry

There is no built-in backoff; that keeps the core predictable. Wrap calls yourself:

```rust,verify
use rig::agent::Agent;
use rig::completion::PromptError;
use std::time::Duration;

/// Boxed: `PromptError` carries the run's history unboxed, which clippy's
/// `result_large_err` rejects in a return type.
async fn prompt_with_retry(
    agent: &Agent,
    input: &str,
    max_attempts: u32,
) -> Result<String, Box<PromptError>> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match agent.prompt(input).max_turns(5).await {
            Ok(reply) => return Ok(reply.output),

            Err(e) if attempt < max_attempts && is_retryable(&e) => {
                let backoff = Duration::from_millis(200 * 2u64.pow(attempt - 1));
                tracing::warn!(attempt, ?backoff, error = %e, "retrying");
                tokio::time::sleep(backoff).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// rig's own classification: a transport failure, a truncated reply, or a
/// provider 408, 425, 429 or 5xx. Anything else, a 401 or `MaxTurnsError`
/// included, fails the same way again.
fn is_retryable(error: &PromptError) -> bool {
    match error {
        PromptError::Report(report) => report.is_retryable(),
        PromptError::CompletionError(error) => error.is_retryable(),
        _ => false,
    }
}
```

Three things this gets right and hand-rolled loops often do not:

- **Retry only what can succeed.** `MaxTurnsError`, `UnknownToolCall` and a provider 4xx
  other than 408 and 429 are deterministic given the same inputs; retrying them just spends
  money.
- **Exponential backoff, bounded.** A fixed 100 ms retry against a rate limit is a
  self-inflicted denial of service.
- **Retry the smallest unit.** Inside a workflow, retry the failing step, not the whole
  chain — otherwise a retry re-runs steps that already succeeded and had side effects.

Add jitter — `backoff + Duration::from_millis(rand::random::<u64>() % 100)` — when many
clients retry at once, or they will synchronize and hammer the provider in lockstep.

### Rate limits

Providers surface rate limits as a 429 reply, so the same backoff handles them. For heavy
workloads, throttle *before* the provider does: put a `tokio::sync::Semaphore` in front of
your agent, or drive batches with
`futures::stream::iter(..).buffer_unordered(n)` at a modest `n`. That converts a burst of
429s into steady throughput.

### Tool timeouts

A tool that hangs blocks the whole run — Rig does not impose a timeout on your code. Wrap
external calls inside the tool:

```rust
let result = tokio::time::timeout(Duration::from_secs(10), fetch(&url))
    .await
    .map_err(|_| ToolExecutionError::timeout("upstream did not respond in 10s"))?;
```

MCP tools are the exception: those are bounded by `DEFAULT_MCP_TOOL_TIMEOUT` unless you
override it.

## Runtime Failures That Compile Fine

- **`OPENAI_API_KEY not set`** — `from_env()` fails at runtime, not compile time. Export
  the key or load a `.env` before running.
- **My tool is never called** — the model chooses from name and description. Vague
  metadata, or too many overlapping tools, makes it skip yours. Verify the wiring with a
  scripted tool-call turn (see Testing) so you know whether
  the problem is your wiring or your description.
- **Extraction fails to deserialize** — the schema is ambiguous or a field is
  under-described. Use `///` doc comments, `Option<T>` for optional values, and enums for
  fixed choices.
- **The agent forgot the conversation** — memory without a conversation id is silently
  bypassed. See Memory.
- **RAG returns irrelevant documents** — the query and the documents were embedded with
  different models, or there is no similarity threshold.

## Production Checklist

- Set `max_turns` deliberately on every tool-using prompt; do not rely on the default.
- Bound every autonomous or evaluator loop with a retry budget.
- Set `max_tokens` on every Claude agent, sized for thinking plus the reply: thinking is
  always on for Claude Opus 5.5 and Fable 5.1 and counts toward the limit, so a cap sized
  for the answer truncates it. Bound cost with `max_turns`, not a small `max_tokens`.
- Put a concurrency limiter in front of the provider.
- Time out external calls inside tools.
- Leave `record_content_telemetry` off unless you have decided the exposure is acceptable.
- Log `PromptError` variants with their turn counts and structural metadata, not full
  prompt content.
- Enforce authorization inside tools and downstream services, not in hooks or prompts.
- Treat model output, tool results, retrieved documents, and MCP responses as untrusted
  data — never as instructions.

Back to the reference index in SKILL.md.
