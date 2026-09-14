# building-rig-agents

### Triggering

**Should load**

1. "Add a `POST /chat` endpoint to our axum service that talks to OpenAI and streams the
   reply back over SSE."
2. "Build an agent with two tools, a typed structured output, and message history kept
   per user."
3. "Our agent calls the tool but then gives up. `Error: MaxTurnsError { max_turns: 1 }` —
   what is wrong?"
4. "I want the model to return a typed struct instead of a string. How do I do that with
   rig?"
5. "Write a test for the assistant that does not hit the provider — a stub completion
   model instead of OpenAI."

**Should not load**

1. "Add an SSE endpoint that streams rows from Postgres to the browser." -> `axum-service` (SSE responses and handler signatures).
2. "`the trait bound ... Handler<_, _> is not satisfied` on my route." -> `axum-service`.
3. "Set up `httpmock` and the `test_app` helper so every API test gets its own database." -> `rust-testing` (and `sea-orm-postgres` for the container).
4. "My spans never reach the OTLP collector and the trace ids do not match across services." -> `axum-service` (tracing and OTLP export).
5. "`cargo deny check` fails on a duplicate `rustls` after the last dependency bump." -> `rust-tooling`.

### Eval 1 - agent behind an axum endpoint

**Prompt**: "Put an agent behind `POST /chat` in our existing axum service. It should
validate the request, map provider failures onto our `AppError`, and not leak the
provider's message to the client."

**Must produce**:
- The agent held as `Arc<Agent>` in `AppState`, built once outside the handler (`Agent` is
  not generic in 0.42, so no type parameter on the state).
- `error.rs` left untouched: one `agent_error(PromptError) -> AppError` function in the chat
  module, called with `.map_err(agent_error)?`, that sends `MaxTurnsError`,
  `UnknownToolCall` and `PromptCancelled` to `AppError::Other` (500), a provider 429
  (`provider_response_status()`) to `AppError::TooManyRequests { retry_after_secs }` (429,
  `Retry-After` parsed from `provider_response_headers()`), and everything else to
  `AppError::Unavailable(error.to_string())` (503).
- The 503 and 500 bodies carrying the existing constants, with the provider's message logged
  by `into_response` and never sent; a test asserting `SERVICE_UNAVAILABLE` and that the
  body does not contain the provider text.
- `max_turns` set explicitly on the prompt.

**Must not produce**:
- A new `AppError` variant (`Agent(#[from] PromptError)` or any other), a new `status()` arm,
  or a `From<PromptError> for AppError` impl.
- A 502 for a provider failure — 502 is reserved for the service's own outbound `Http` calls.
- `error.to_string()` or the provider's message in a response body.
- `Agent<M>` or a generic parameter threaded through `AppState`.
- A new agent constructed inside the handler.
- Hand-rolled SSE plumbing when the task did not ask for streaming.

### Eval 2 - a tool that the agent actually calls

**Prompt**: "Give the agent a tool that looks up an order by id and returns its status.
It compiles but the model never calls it."

**Must produce**:
- `#[rig::tool_macro(description = "...")]`, with the generated `PascalCase` type attached
  via `.tool(..)`.
- A turn budget of at least two (`.max_turns(n)` or `.default_max_turns(n)`), with the
  reason: the default budget is a single model call, which cannot fit a tool call and an
  answer.
- A model-facing description and parameter docs, plus tool errors phrased so the model can
  recover, because a returned `Err` is fed back into the loop rather than aborting it.

**Must not produce**:
- A `definition()` method or `ToolError` (both are pre-0.42 shapes).
- `dynamic_tools(n, index, toolset)` for vector-retrieved tools — that is
  `retrieved_tools` in 0.42.
- A claim that the tool is not called because of a rig bug, before the description and the
  turn budget have been checked.

### Eval 3 - offline test of a multi-turn run

**Prompt**: "Write a test that proves the agent calls the `add` tool and then answers,
without touching a provider."

**Must produce**:
- `rig = { version = "0.42", features = ["test-utils"] }` under `[dev-dependencies]`.
- `MockCompletionModel::from_turns([...])` scripting a `MockTurn::tool_call` followed by a
  `MockTurn::text`, and `AgentBuilder::new(model)` rather than a provider client.
- A clone of the mock used as a probe, asserting `request_count() == 2`.
- `.max_turns(3)` on the prompt.

**Must not produce**:
- `test-utils` in `[dependencies]`.
- An `OPENAI_API_KEY`, a network call, or a `#[ignore]`d test.
- A mock built by hand-implementing `CompletionModel` when `rig::test_utils` covers it.

### Eval 4 - an agent that forgets

**Prompt**: "I set `.memory(InMemoryConversationMemory::new())` but the agent still does not
remember the previous message."

**Must produce**:
- The diagnosis: without a conversation id, memory is silently bypassed — no error, no
  warning. Fix with `.conversation("...")` per request or
  `AgentBuilder::conversation(..)` as a default.
- The neighbouring traps if history is also being passed: `history(..)` bypasses memory in
  both directions and does not record the turn, whereas `chat(prompt, &mut history)`
  appends the committed turn (so it must not be pushed again).

**Must not produce**:
- `with_history(..)`, which was renamed to `history(..)`.
- Advice to persist history manually before the conversation id has been checked.
