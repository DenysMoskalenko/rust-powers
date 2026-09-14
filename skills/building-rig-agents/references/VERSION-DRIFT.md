# Version Drift: rig.rs Docs vs Released 0.42

Read this file before copying any code sample from <https://rig.rs/docs>, and whenever a
snippet that "should work" does not compile.

## Contents

- [The Situation](#the-situation)
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
- **<https://docs.rs/rig/0.42.0>** — the released API. Exhaustive and always correct for
  the version you actually depend on.

The Rig maintainers say as much: several pages carry a "Main branch API" banner, and the
Cargo snippets on the site still say `rig = "0.39.0"`.

**Working rule: take the concepts from rig.rs, take the signatures from docs.rs.** When
they disagree, docs.rs wins — it is generated from the code you compiled against.

Rig also ships a `MIGRATING.md` in the repository covering every breaking change from 0.30
onward, including a "Silent behavior changes" section for changes that still compile. Read
the section for the version you are on. Note that its newest section, `0.41 → next`, mixes
changes that shipped in 0.42 with changes still unreleased — do not treat it as a
description of 0.42 alone.

## The Divergences That Bite

### 1. `Agent` is no longer generic

```rust,ignore
// rig.rs / ≤0.41
let agent: Agent<openai::responses_api::ResponsesCompletionModel> = ...;
struct Router { routes: HashMap<String, Agent<ResponsesCompletionModel>> }

// 0.42
let agent: Agent = ...;
struct Router { routes: HashMap<String, Agent> }
```

`AgentBuilder::new(model)` erases the model into a `ModelHandle` at construction. This is a
simplification, not a loss: a single `HashMap<String, Agent>` now holds agents from
different providers, so the enum-dispatch and provider-registry patterns the website
prescribes for runtime provider selection are obsolete. Swap models at runtime with
`set_model`, `set_model_handle`, `with_model_handle`, or per-run `using_model`.

### 2. `OneOrMany<T>` is gone

```rust,ignore
// rig.rs
Message::Assistant { id: None, content: OneOrMany::one(AssistantContent::text("hi")) }

// 0.42
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

// 0.42
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

// 0.42
Err(rig::tool::ToolExecutionError::invalid_args("divide requires a non-zero y"))
```

`ToolExecutionError` is one envelope with kind constructors (`invalid_args`, `timeout`,
`rate_limited`, `permission_denied`, `network`, `provider`, `not_found`, `cancelled`,
`other`) and separate operator-facing and model-facing messages.

### 5. Hooks: no `StepEvent`, no `Flow`

```rust,ignore
// rig.rs (unreleased main)
impl<M: CompletionModel> AgentHook<M> for MyHook {
    async fn on_event(&self, event: StepEvent<'_, M>) -> Flow {
        match event { StepEvent::ToolCall { .. } => Flow::skip("no"), _ => Flow::cont() }
    }
}

// 0.42
impl AgentHook for MyHook {
    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        ToolCallAction::skip("no")
    }
}
```

0.42 has per-event methods, each returning its own action type. All methods are provided,
so you implement only what you need. `RequestOverride` is `RequestPatch`;
`Flow::override_request` is `CompletionCallAction::patch`. `StepEventKind` still exists,
for `observes`.

### 6. `dynamic_tools` changed meaning

```rust,ignore
// rig.rs — tool-RAG
.dynamic_tools(2, tool_index, toolset)

// 0.42 — tool-RAG
.retrieved_tools(2, tool_index, toolset)
```

In 0.42, `dynamic_tools(Vec<DynamicTool>)` registers tools whose name and callback are
known only at runtime. Vector-retrieved tools are `retrieved_tools(n, index, toolset)`.

Related: `ToolSet::builder()` no longer exists. Use `ToolSet::default()` plus
`add_tool` / `add_retrieved_tool` / `add_dynamic_tool`, or `ToolSet::from_tools(vec![..])`.

### 7. `rig::evals` does not exist

The Evals page on rig.rs documents `Eval`, `EvalOutcome`, `LlmJudgeBuilder`,
`LlmScoreMetric`, and `SemanticSimilarityMetric` behind an `experimental` feature. That
module shipped in 0.39 and was **removed by 0.41**. `rig` 0.42 has no `experimental`
feature and no `evals` module — `cargo add rig -F experimental` fails.

Build judges and scorers on extractors instead; see
Testing and Debugging.

### 8. `max_turns` counts differently than the prose implies

rig.rs describes the default as "the initial request plus one follow-up". In 0.42 it is a
**total model-call budget**: zero permits no model call, one permits only the initial call.
With no `default_max_turns` configured the implicit budget is one — so a tool call followed
by an answer needs at least two, and any tool-using prompt needs an explicit
`.max_turns(n)`.

### 9. Vector stores are features, not separate crates

rig.rs describes `rig-mongodb`, `rig-lancedb`, `rig-qdrant`, and friends as companion
crates you add alongside `rig`. The `rig` 0.42 facade re-exports them as feature-gated
modules:

```toml
rig = { version = "0.42", features = ["qdrant", "lancedb"] }
```

The companion crates still exist and still work; the feature is usually simpler.

### 10. Builder method names

| rig.rs | 0.42 |
|---|---|
| `AgentBuilder::conversation_id(..)` | `AgentBuilder::conversation(..)` |
| `tool_extensions(..)` | `tool_context(..)` on `PromptRequest` / `AgentRunner`, not on `AgentBuilder` |
| `with_history(..)` on a prompt request | `history(..)` |

### 11. Model ids in examples

Website samples use `"gpt-5.5"`; the docs.rs examples use constants such as
`openai::GPT_5_2`. Both are fine — a model id is just a string — but verify the id against
your provider's current catalog rather than trusting either doc set. Model names age faster
than APIs.

## Also Worth Knowing

- The experimental `pipeline` module (`Op`, `pipeline::new`, `parallel!`) has been removed.
  Workflows are plain `async` Rust; see Orchestration.
- MCP support is on the `rmcp` feature of `rig` in 0.42. Unreleased `main` moves it to a
  separate `rig-rmcp` crate — expect this to change in a future release.
- `providers::llamafile` is still `llamafile` in 0.42. The rename to `providers::llamacpp`
  is in `MIGRATING.md`'s `0.41 → next` section, i.e. unreleased — one more reason not to
  read that section as a description of 0.42.
- `record_content_telemetry` (default `false`) is newer than most website prose, which
  predates the opt-in.
- An `Agent` used as another agent's tool goes through `Agent::into_tool()` →
  `DynamicTool` → `.dynamic_tool(..)`. `Agent` does not implement `Tool`, so `.tool(agent)`
  does not compile.
- `CompletionRequest::preamble` is a legacy field that request construction always leaves
  `None`; the preamble is prepended to `chat_history` as `Message::System`. Tests that
  assert on the field silently pass.

## How to Check Something Yourself

1. Open `https://docs.rs/rig/0.42.0/rig/all.html` and search for the symbol. Absent means
   it does not exist in this release.
2. Open the item page for the exact signature, and check for an "Available on crate
   feature `x` only" banner.
3. If it is missing, check `MIGRATING.md` in the repository for the replacement.
4. Compile the snippet. `cargo check` settles every disagreement between doc sets.

When you write Rig code for someone, prefer the API you can point at on docs.rs for their
version. A snippet that reads well and does not compile costs more than no snippet.

Back to the reference index in SKILL.md.
