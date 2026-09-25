# building-rig-agents

### Triggering

**Should load**

1. "Our support page needs a chat box: the backend should forward the user's message to OpenAI and stream the answer back token by token."
2. "I need an assistant that looks up orders and refunds by itself, answers in a typed struct, and remembers what each user said earlier."
3. "Our agent calls the tool but then gives up. `Error: MaxTurnsError { max_turns: 1 }` — what is wrong?"
4. "I want the model to return a typed struct instead of a string. How do I do that with rig?"
5. "Write a test for the assistant that does not hit the provider — a stub completion model instead of OpenAI."
6. "`error[E0599]: no method named `agent` found for struct `Client` in the current scope`, right after I added rig to Cargo.toml."
7. "Let the agent answer questions from our product docs: embed the markdown files and pull the most relevant ones into each prompt."
8. "Connect our agent to the company's MCP server so it can use the tools listed there."

**Should not load**

1. "Add an SSE endpoint that streams rows from Postgres to the browser." -> `axum-service`
2. "Rate-limit `POST /chat` to ten requests a minute per user, across every replica." -> `rust-redis`
3. "Cache the model's answers in Redis for an hour, keyed by a hash of the prompt." -> `rust-redis`
4. "When a chat run finishes, publish an `agent.completed` event to JetStream for the billing service." -> `rust-nats`
5. "Write the sea-orm query that pages through one conversation's stored messages, newest first, with a total count." -> `sea-orm-postgres`
6. "Mock the payment provider's HTTP API with `httpmock` in our axum API tests." -> `rust-testing`
7. "Add a Prometheus counter for `/chat` requests by outcome and expose it on `/metrics`." -> `axum-service`
8. "`cargo deny` rejects the licence of a crate that `rig` pulls in. How do I allow it?" -> `rust-tooling`
9. "Return 503 with a constant body when the payment provider is down, without adding an `AppError` variant." -> `axum-service`

### Eval 1 - agent behind an axum endpoint

**Prompt**: "Put an agent behind `POST /chat` in our existing axum service. It should validate the request, map provider failures onto our `AppError`, and not leak the provider's message to the client."

**Must produce**:

- The agent held as `Arc<Agent>` in `AppState`, built once outside the handler.
- The mapping is one `agent_error(PromptError) -> AppError` function in the chat module, called with `.map_err(agent_error)?`.
- `MaxTurnsError` and `UnknownToolCall` mapped to `AppError::Other` (500).
- A provider 400, 401 or 403 (`provider_response_status()`) mapped to `AppError::Other` (500).
- `PromptCancelled` carrying the constant reason every policy hook stops with mapped to `AppError::BadRequest` (400) with a constant message.
- Any other `PromptCancelled`, which rig also raises for its own failures, mapped to `AppError::Other` (500).
- A provider 429 mapped to `AppError::TooManyRequests { retry_after_secs }`.
- `retry_after_secs` parsed from the provider's `Retry-After` header (`provider_response_headers()`).
- Every other failure mapped to `AppError::Unavailable(error.to_string())` (503).
- The provider's message logged, never sent: the 500 and 503 bodies stay the existing constants.
- A test asserting that a provider outage is answered 503.
- A test asserting that the error body does not contain the provider's message.
- `max_turns` set explicitly on the prompt.

**Must not produce**:

- A new `AppError` variant (`Agent(#[from] PromptError)` or any other).
- A new `status()` arm in `error.rs`.
- A `From<PromptError> for AppError` impl.
- A 502 for a provider failure: 502 is reserved for the service's own outbound `Http` calls.
- A provider 401 or 403 answered 503.
- `error.to_string()` or the provider's message in a response body.
- `Agent<M>` or a generic parameter threaded through `AppState`.
- An agent constructed inside the handler.
- Hand-rolled SSE plumbing when the task did not ask for streaming.

### Eval 2 - a tool the model never calls

**Prompt**: "Our support agent is built in `main` as `client.agent(MODEL).preamble(PREAMBLE).build()` and called with `agent.prompt(question).await?`. Give it a tool that looks up an order by id through `orders::status(&db, id)` and returns its status. Right now it answers order questions from memory and never calls the tool."

**Fixture**: empty

**Must produce**:

- The tool as `#[rig::tool_macro(description = "...")]` with its generated `PascalCase` type, or as a hand-written `Tool` impl with `description()` and `parameters()`.
- The tool registered on the agent builder with `.tool(..)`.
- A tool description written for the model that says when to use the tool.
- A turn budget of at least two (`.max_turns(n)` or `.default_max_turns(n)`), because a tool call plus an answer is two model calls.
- Tool errors phrased so the model can recover, because a returned `Err` is fed back into the loop rather than aborting it.

**Must not produce**:

- A `definition()` method on the tool.
- `ToolError`, the pre-0.42 error type.
- A claim that the tool is ignored because of a rig bug, before the description and the registration have been checked.

### Eval 3 - offline test of a multi-turn run

**Prompt**: "Write a test that proves the agent calls the `add` tool and then answers, without touching a provider."

**Must produce**:

- `rig = { version = "0.42", features = ["test-utils"] }` under `[dev-dependencies]`.
- `MockCompletionModel::from_turns([...])` scripting a `MockTurn::tool_call` followed by a `MockTurn::text`.
- `AgentBuilder::new(model)` in place of a provider client.
- A clone of the mock used as a probe, asserting two model requests (`request_count() == 2` or `requests().len() == 2`).
- An explicit `.max_turns(n)` of at least 2 on the prompt.

**Must not produce**:

- `features = ["test-utils"]` on the `rig` line under `[dependencies]` rather than `[dev-dependencies]`.
- An `OPENAI_API_KEY` or any network call to a provider.
- A `#[ignore]`d test.
- A mock built by hand-implementing `CompletionModel` when `rig::test_utils` covers it.

### Eval 4 - an agent that forgets

**Prompt**: "I set `.memory(InMemoryConversationMemory::new())` but the agent still does not remember the previous message."

**Fixture**: empty

**Must produce**:

- The diagnosis: without a conversation id, memory is silently bypassed, with no error and no warning.
- The fix: `.conversation("...")` per request, or `AgentBuilder::conversation(..)` as a default.
- `history(..)` on the same request bypasses memory: nothing is loaded and the turn is not recorded.

**Must not produce**:

- `with_history(..)`, which was renamed to `history(..)`.
- Advice to persist history manually before the conversation id has been checked.

### Eval 5 - streaming the reply

**Prompt**: "Stream the chat reply over SSE from `POST /chat/stream`, and make sure a model that hangs cannot hold the connection open forever."

**Must produce**:

- The stream route merged inside the middleware stack, like every other route.
- The stream bounded from inside, by wrapping it: `StreamExt::timeout` per item, or a deadline.
- When that bound fires, a final `error` event, after which the stream ends without polling the run again.
- `.max_turns(n)` on `stream_prompt`.
- A mid-stream failure sent as a final `error` event, after which the stream ends.

**Must not produce**:

- A timeout whose `Elapsed` only becomes an event while the stream keeps waiting on the stalled run.
- The stream route merged after `.layer(..)` to keep `TimeoutLayer` off it.
- The claim that `TimeoutLayer` cuts a stream that has already started.
- The provider's error text in an SSE event.

### Probe 1 - the facade crate

**Prompt**: "Add rig to my Cargo.toml and build an OpenAI agent with one calculator tool."

**Fixture**: empty

**Wrong answer**: `rig-core\s*=|\.multi_turn\s*\(\s*\d`

**Right answer**: `\brig\s*=\s*[{"]`

### Probe 2 - a hand-written tool

**Prompt**: "Implement a rig `Tool` by hand, without the macro, that adds two integers."

**Wrong answer**: `fn\s+definition\s*\(|ToolDefinition\s*\{|ToolError::`

**Right answer**: `fn\s+parameters\s*\(`

### Probe 3 - retrieved tools

**Prompt**: "My rig agent has 150 tools; offer only the 3 most relevant to each request, picked through a vector index."

**Wrong answer**: `\.dynamic_tools\s*\(\s*\d`

**Right answer**: `\.retrieved_tools\s*\(`

### Probe 4 - the chat() turn budget

**Prompt**: "Keep the conversation in my own `Vec<Message>` and give the agent a lookup tool."

**Wrong answer**: `\.chat\([^;]*\)\s*\.max_turns\s*\(\s*\d+\s*\)\s*\.await`

**Right answer**: `\.default_max_turns\s*\(`

### Probe 5 - a caller-scoped conversation id

**Prompt**: "Let users continue a conversation by sending a `conversation_id` with each `POST /chat`; the agent already has `.memory(..)`."

**Wrong answer**: `\.conversation\(\s*&?\s*(?:body|req|request|payload|input|params)\.conversation_id\b|(?:Some\(|let\s+(?:mut\s+)?)\s*(\w+)\s*\)?\s*=\s*&?\s*(?:\w+\.)?conversation_id\b(?:(?!let\s+(?:mut\s+)?\1\b)[\s\S]){0,400}?\.conversation\(\s*&?\s*\1(?:\.clone\(\))?\s*\)`

### Probe 6 - no forced tool choice on Claude Opus 5.5

**Prompt**: "Our support agent runs on `claude-opus-5-5` through rig's Anthropic client and sometimes answers without looking the order up. Make it call `lookup_order` before every answer."

**Wrong answer**: `\.tool_choice\(\s*(?:rig::message::)?ToolChoice::(?:Required|Specific)`
