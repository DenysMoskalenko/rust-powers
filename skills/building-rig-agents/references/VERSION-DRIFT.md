# Version Drift: rig.rs Docs vs Released 0.43

Read this file before copying any code sample from <https://rig.rs/docs>, and whenever a
snippet that "should work" does not compile.

## Contents

- [The Situation](#the-situation)
- [0.42 → 0.43](#042--043)
- [The Divergences That Bite](#the-divergences-that-bite)
  - [1. `Agent` is no longer generic](#1-agent-is-no-longer-generic)
  - [2. `OneOrMany<T>` is gone](#2-oneormanyt-is-gone)
  - [3. The `Tool` trait was restructured](#3-the-tool-trait-was-restructured)
  - [4. Tool errors: `ToolError` → `ToolExecutionError`](#4-tool-errors-toolerror-toolexecutionerror)
  - [5. Hooks: no `StepEvent`, no `Flow`](#5-hooks-no-stepevent-no-flow)
  - [6. `dynamic_tools` changed meaning](#6-dynamic_tools-changed-meaning)
  - [7. `rig::evals` does not exist](#7-rigevals-does-not-exist)
  - [8. `max_turns` counts differently than the prose implies](#8-max_turns-counts-differently-than-the-prose-implies)
  - [9. Vector stores are features, not separate crates](#9-vector-stores-are-features-not-separate-crates)
  - [10. Builder method names](#10-builder-method-names)
  - [11. Model ids in examples](#11-model-ids-in-examples)
- [Also Worth Knowing](#also-worth-knowing)
- [How to Check Something Yourself](#how-to-check-something-yourself)

## The Situation

Rig has two documentation surfaces and they are not in sync:

- **<https://rig.rs/docs>** — the narrative guide. Excellent on concepts, mental models,
  and *why* to reach for something. Its code samples were written across roughly 0.38–0.41
  plus a few pages describing unreleased `main`, and several no longer compile.
- **<https://docs.rs/rig/0.43.0>** — the released API. Exhaustive and always correct for
  the version you actually depend on.

The Rig maintainers say as much: several pages carry a "Main branch API" banner, and the
Cargo snippets on the site still say `rig = "0.39.0"`.

**Working rule: take the concepts from rig.rs, take the signatures from docs.rs.** When
they disagree, docs.rs wins — it is generated from the code you compiled against.

Rig also ships a `MIGRATING.md` in the repository covering every breaking change from 0.30
onward, including a "Silent behavior changes" section for changes that still compile. Read
the section for the version you are on; in 0.43.0 the newest is `0.41 → next`, which covers
0.42 and 0.43.

## 0.42 → 0.43

0.43 rebuilt the provider layer and the run surface, so most 0.42 code no longer compiles.
Old spelling on the left; where the old one still compiles, the right cell says what it now
does.

| 0.42 | 0.43 |
|---|---|
| `openai::Client::from_env()?` | `OpenAI::from_env()?` (`use rig::providers::openai::{self, OpenAI}`); `Anthropic`, `Gemini` likewise. OpenAI-compatible vendors (DeepSeek, Groq, xAI, OpenRouter, …) have no client type: `deepseek::from_env()?` returns an `OpenAI` |
| `openai::Client::new(key)?` | `OpenAI::new(key)` — infallible |
| `Client::builder().api_key(k).base_url(u).build()?` | `OpenAIConfig::new(k).with_base_url(u).client()` |
| `.http_client(h)` on the client builder | `.with_http(h)` on any client |
| `client.agent(m)` | `AgentBuilder::new(client.completion(m))` |
| `client.extractor::<T>(m)` | `ExtractorBuilder::<T>::new(client.completion(m))` |
| `client.completion_model(m)` / `client.embedding_model(m)` | `client.completion(m)` / `client.embedding(m, None)` |
| `use rig::prelude::*;` to bring methods into scope | the methods are inherent; the prelude re-exports types (`AgentBuilder`, `Agent`, …) and the `Tool`, `PortableTool` and `VectorStoreIndex` traits (`top_n` needs the last in scope) — where nothing else from it is used, it is now an unused import |
| `agent.prompt(p).await?` → `String` | `agent.prompt(p).await?.output`; the run returns a `PromptResponse` |
| `agent.chat(p, &mut h).await?` → `String` | `.await?.output`; `chat` still appends the committed turn to `h` |
| `.extended_details()` | gone: every run returns the `PromptResponse` |
| `agent.runner(p)`, `PromptRequest` | `agent.prompt(p)` is the `AgentRunner` |
| `agent.stream_prompt(p).await` / `agent.stream_chat(p, &h).await` | `agent.prompt(p).stream()` / `agent.prompt(p).history(&h).stream()` — synchronous, lazy |
| `StreamedAssistantContent::Text(t)` → `t.text` | `Item::Event(StreamEvent::Text { text, .. })` |
| `StreamedAssistantContent::ToolCall` / `ToolCallDelta` | `MultiTurnStreamItem::ToolCall { tool_call }`; arguments arrive once, as `StreamEvent::Arguments` |
| `StreamingError::Prompt(Box<PromptError>)` | `StreamingError::Prompt(PromptError)`, unboxed |
| `PauseControl` | stop polling to pause, drop the stream to cancel |
| `Prompt`, `Chat`, `TypedPrompt`, `StreamingPrompt`, `StreamingChat` imports | delete them: the methods are inherent on `Agent` |
| `#[rig::tool_macro]` | `#[rig::rig_tool]` |
| `on_tool_call` → `ToolCallAction` | `on_dispatch` → `DispatchAction` (`proceed`, `skip`, `stop`, `rewrite_tool_args`), called for every tool call and completion (other effects only when `observes` opts in): gate on `event.tool_name()` |
| `on_tool_result` → `ToolResultAction`, `on_completion_response`, `on_stream_response_finish` | `on_outcome` → `OutcomeAction` (`proceed`, `rewrite_tool_result`, `stop`); `event.completion()` is the model reply |
| `StepEventKind::ToolCall` / `ToolResult` | `StepEventKind::ToolDispatch`; for dispatch events `observes` now decides whether the hook is called |
| `.rmcp_tools(tools, peer)` / `rmcp_tool(..)` | `.dynamic_tools(..)` over `rig::tool::rmcp::tools_from_server(tools, mcp.peer())`, each `McpTool` converted with `DynamicTool::from` |
| `rmcp_tools_with_timeout(..)` | `McpTool::with_timeout(..)` before the conversion |
| any `Clone` type in a `ToolContext` | `#[derive(Serialize, Deserialize, ContextValue)]`; `insert`, `get` and `require` return a `Result` and `get` an owned value; `get_mut` is gone |
| `DynamicTool::new(.., \|ctx, args\| ..)` | `DynamicTool::new(.., \|args\| ..)`, or `new_with_context` |
| `CompletionError` (`HttpError`, `ProviderError(..)`, …) | `ProviderError` (`Http`, `Provider(..)`, …), one type for every operation; a non-2xx reply is `ProviderResponse`, never `Http` |
| a provider failure as `PromptError::CompletionError` | usually `PromptError::Report(ErrorReport)`; the `provider_response_*` accessors read it the same way |
| `usage.input_tokens: u64`, zero meaning "not reported" | `Option<u64>`, `None` meaning "not reported"; Anthropic `input_tokens` now includes cache reads and writes |
| `extractor.extract(t).await?` → `T` | `.await?.output` |
| `extract_with_usage(t)` / `extract_with_chat_history(t, h)` | `extract(t).await?` (`.usage`) / `extract(t).history(h)` |
| `ExtractionError::NoData` | `StructuredOutputError::EmptyResponse` |
| `.preamble(..)` on the extractor builder | `.append_preamble(..)`; the extraction preamble is fixed |
| `prompt_typed::<T>(p).await?` → `T` | `.await?.output` |
| `set_model_handle` / `with_model_handle`, `using_model(handle)` | `set_model(model)` / `with_model(model)`, `using_model(label)` for a model registered with `model_route(label, model)` |
| `MockEmbeddingModel` as a value | `MockEmbeddings::model()`; `MockEmbeddingModel` is a type alias |
| `impl CompletionModel for MyModel`, `M: CompletionModel` bounds | `impl Into<DynModel<Completion>>`; a custom model implements `Wire` |
| `model.completion_request(p)….send().await?` | `model.call(CompletionRequest::new(p)…).await?` |
| `CompletionRequest::preamble` (the field) | removed; `request.system_instructions()` |
| `ConversationMemory::load(&str)` | `load(&ConversationId)` |
| `HeuristicTokenCounter::openai()` | `HeuristicTokenCounter::default()` |
| `toolset.add_retrieved_tool(t)` → `String` | `-> Result<String, serde_json::Error>`; ignoring it is an unused-`Result` warning |
| `rig::client::RerankingClient` | a client's own `rerank(model)` |
| `providers::llamafile` | `providers::llamacpp` |

Behavior that changed with no compile error:

- A Claude agent without `max_tokens` now works on Claude 5 ids (Opus 5 and 5.5, Sonnet 5 and
  5.5, Fable 5 and 5.1, and their `-20…` dated snapshots), which 0.42 failed: they get 128K,
  and Sonnet 4.6 moves from 64K to 128K. Other Claude 4 ids keep their 0.42 default; any other
  id still fails every prompt with "`max_tokens` must be set for Anthropic".
- The extractor no longer forces its submit tool on Claude Opus 5.5, Sonnet 5.5 and Fable
  5.1; it asks for native output there. A forced `ToolChoice` you set on an agent is still
  sent as is.
- OpenAI Responses tools are sent with `strict: false`.
- A stream cut before the provider ended it is `ProviderError::Truncated`, not a short
  answer.

## The Divergences That Bite

### 1. `Agent` is no longer generic

```rust,ignore
// rig.rs / ≤0.41
let agent: Agent<openai::responses_api::ResponsesCompletionModel> = ...;
struct Router { routes: HashMap<String, Agent<ResponsesCompletionModel>> }

// 0.42 and 0.43
let agent: Agent = ...;
struct Router { routes: HashMap<String, Agent> }
```

`AgentBuilder::new(model)` erases the model into a `DynModel` at construction. This is a
simplification, not a loss: a single `HashMap<String, Agent>` now holds agents from
different providers, so the enum-dispatch and provider-registry patterns the website
prescribes for runtime provider selection are obsolete. Swap models at runtime with
`set_model`, `with_model`, or per-run `using_model_value` / `using_model(label)`.

### 2. `OneOrMany<T>` is gone

```rust,ignore
// rig.rs
Message::Assistant { id: None, content: OneOrMany::one(AssistantContent::text("hi")) }

// 0.42 and 0.43
Message::Assistant { id: None, content: vec![AssistantContent::text("hi")] }
```

Content lists are plain `Vec<T>`. `Message` also gained a third variant,
`System { content: String }`, so any exhaustive `match` on `Message` written against the
old two-variant enum needs a new arm. Prefer `Message::user(..)` / `Message::assistant(..)`
and avoid the issue.

Embeddings changed the same way: `(D, Vec<Embedding>)`, not `(D, OneOrMany<Embedding>)`.

### 3. The `Tool` trait was restructured

```rust,ignore
// rig.rs
async fn definition(&self, _prompt: String) -> ToolDefinition {
    ToolDefinition { name: "add".into(), description: "...".into(), parameters: json!({...}) }
}
async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> { ... }

// 0.42 and 0.43
fn description(&self) -> String { "...".to_string() }
fn parameters(&self) -> serde_json::Value { json!({...}) }
async fn call(&self, ctx: &mut ToolContext, args: Self::Args)
    -> Result<Self::Output, Self::Error> { ... }
```

`definition()` split into `description()` + `parameters()`; the name is `const NAME` only.
`call` takes `&mut ToolContext` first. `type Output` is now bounded by `IntoToolOutput`
(every owned serializable value qualifies). A provided `map_error` lets you classify your
domain error.

`PortableTool` is the context-free variant — same shape, `call(&self, args)` — and blanket-
implements `Tool`. Prefer it for pure functions.

### 4. Tool errors: `ToolError` → `ToolExecutionError`

```rust,ignore
// rig.rs
Err(rig::tool::ToolError::ToolCallError("Division by zero".into()))

// 0.42 and 0.43
Err(rig::tool::ToolExecutionError::invalid_args("divide requires a non-zero y"))
```

`ToolExecutionError` is one envelope with kind constructors (`invalid_args`, `timeout`,
`rate_limited`, `permission_denied`, `network`, `provider`, `not_found`, `cancelled`,
`other`) and separate operator-facing and model-facing messages.

### 5. Hooks: no `StepEvent`, no `Flow`

```rust,ignore
// rig.rs (0.40, replaced in 0.41)
impl<M: CompletionModel> AgentHook<M> for MyHook {
    async fn on_event(&self, event: StepEvent<'_, M>) -> Flow {
        match event { StepEvent::ToolCall { .. } => Flow::skip("no"), _ => Flow::cont() }
    }
}

// 0.43
impl AgentHook for MyHook {
    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        if event.tool_name().is_some() { DispatchAction::skip("no") } else { DispatchAction::proceed() }
    }
}
```

0.43 has per-event methods, each returning its own action type. All methods are provided,
so you implement only what you need. `RequestOverride` is `RequestPatch`;
`Flow::override_request` is `CompletionCallAction::patch`. `StepEventKind` still exists,
for `observes`.

### 6. `dynamic_tools` changed meaning

```rust,ignore
// rig.rs — tool-RAG
.dynamic_tools(2, tool_index, toolset)

// 0.42 and 0.43 — tool-RAG
.retrieved_tools(2, tool_index, toolset)
```

Since 0.41, `dynamic_tools(Vec<DynamicTool>)` registers tools whose name and callback are
known only at runtime. Vector-retrieved tools are `retrieved_tools(n, index, toolset)`.

Related: `ToolSet::builder()` no longer exists. Use `ToolSet::default()` plus
`add_tool` / `add_retrieved_tool` / `add_dynamic_tool`, or `ToolSet::from_tools(vec![..])`.

### 7. `rig::evals` does not exist

The Evals page on rig.rs documents `Eval`, `EvalOutcome`, `LlmJudgeBuilder`,
`LlmScoreMetric`, and `SemanticSimilarityMetric` behind an `experimental` feature. That
module shipped in 0.39 and was **removed by 0.41**. `rig` 0.43 has no `experimental`
feature and no `evals` module — `cargo add rig -F experimental` fails.

Build judges and scorers on extractors instead; see
Testing and Debugging.

### 8. `max_turns` counts differently than the prose implies

rig.rs describes the default as "the initial request plus one follow-up". Since 0.40 it is a
**total model-call budget**: zero permits no model call, one permits only the initial call.
With no `default_max_turns` configured the implicit budget is one — so a tool call followed
by an answer needs at least two, and any tool-using prompt needs an explicit
`.max_turns(n)`.

### 9. Vector stores are features, not separate crates

rig.rs describes `rig-mongodb`, `rig-lancedb`, `rig-qdrant`, and friends as companion
crates you add alongside `rig`. The `rig` 0.43 facade re-exports them as feature-gated
modules:

```toml
rig = { version = "0.43", features = ["qdrant", "lancedb"] }
```

The companion crates still exist and still work; the feature is usually simpler.

### 10. Builder method names

| rig.rs | 0.43 |
|---|---|
| `AgentBuilder::conversation_id(..)` | `AgentBuilder::conversation(..)` |
| `tool_extensions(..)` | `tool_context(..)` on the `AgentRunner`, not on `AgentBuilder` |
| `with_history(..)` on a prompt request | `history(..)` |

### 11. Model ids in examples

Website samples use `"gpt-5.5"`; the docs.rs examples use constants such as
`openai::GPT_5_2`. Both are fine — a model id is just a string — but verify the id against
your provider's current catalog rather than trusting either doc set. Model names age faster
than APIs.

## Also Worth Knowing

- The experimental `pipeline` module (`Op`, `pipeline::new`, `parallel!`) has been removed.
  Workflows are plain `async` Rust; see Orchestration.
- MCP support is on the `rmcp` feature of `rig`, which since 0.43 pulls in the separate
  `rig-rmcp` crate, reached as `rig::tool::rmcp`.
- `record_content_telemetry` (default `false`) is newer than most website prose, which
  predates the opt-in.
- An `Agent` used as another agent's tool goes through `Agent::into_tool()` →
  `DynamicTool` → `.dynamic_tool(..)`. `Agent` does not implement `Tool`, so `.tool(agent)`
  does not compile.

## How to Check Something Yourself

1. Open `https://docs.rs/rig/0.43.0/rig/all.html` and search for the symbol. Absent means
   it does not exist in this release.
2. Open the item page for the exact signature, and check for an "Available on crate
   feature `x` only" banner.
3. If it is missing, check `MIGRATING.md` in the repository for the replacement.
4. Compile the snippet. `cargo check` settles every disagreement between doc sets.

When you write Rig code for someone, prefer the API you can point at on docs.rs for their
version. A snippet that reads well and does not compile costs more than no snippet.

Back to the reference index in SKILL.md.
