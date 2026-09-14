# Hooks and Runner

Read this file when the user wants per-run controls, audit logging, approval flows,
guardrails, request shaping, invalid-tool-call recovery, or concurrent tool execution.

Verified against `rig` 0.42.0. The `on_event(StepEvent) -> Flow` hook API documented on
rig.rs is **unreleased**; 0.42 uses per-event methods with per-event action types. See
Version Drift.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [The Three Layers](#the-three-layers)
- [Basic Runner Usage](#basic-runner-usage)
- [Per-Run Controls](#per-run-controls)
- [Writing a Hook](#writing-a-hook)
- [Where Hooks Attach](#where-hooks-attach)
- [Hook Composition](#hook-composition)
- [Guardrails and Approvals](#guardrails-and-approvals)
- [Request Patches](#request-patches)
- [Invalid Tool Calls](#invalid-tool-calls)
- [High-Frequency Events](#high-frequency-events)
- [Best Practices](#best-practices)
- [`AgentRunner` vs `AgentRun`](#agentrunner-vs-agentrun)

## The Three Layers

- **`Agent`** holds reusable configuration: model, preamble, tools, RAG context, memory,
  and default hooks.
- **`AgentRunner`** drives one prompt through the agent loop and owns per-run options —
  turn limits, memory behavior, tool concurrency, tool context, hooks.
- **`AgentRun`** is the lower-level state machine, sans-IO: it decides what should happen
  next but performs no requests itself, so it can be serialized mid-run. Reach for it only
  when you need to pause and resume the loop across processes, e.g. a durable approval.

Use `agent.prompt(..)` for normal calls; it is backed by the same driver. Use
`agent.runner(..)` when you want an explicit value you can build in stages or share
between the blocking and streaming surfaces.

## Basic Runner Usage

```rust
use rig::prelude::*;
use rig::providers::openai;

let agent = openai::Client::from_env()?
    .agent(MODEL)
    .preamble("You are a helpful assistant.")
    .build();

let response = agent
    .runner("Check inventory, shipping, and pricing, then summarize.")
    .max_turns(5)
    .tool_concurrency(3)
    .run()
    .await?;

println!("answer: {}", response.output);
println!("model calls: {}", response.completion_calls.len());
println!("tokens: {}", response.usage.total_tokens);
```

This is equivalent in spirit to `agent.prompt(..).max_turns(5).extended_details().await?`.
`PromptRequest` and `AgentRunner` expose the same option surface; pick whichever reads
better.

## Per-Run Controls

| Method | Effect |
|---|---|
| `max_turns(n)` | Total model-call budget, including the initial call and every retry |
| `max_invalid_tool_call_retries(n)` | Retry budget for invalid-tool-call recovery; retries also consume the normal turn budget |
| `history(iter)` | Explicit history — **bypasses conversation memory for this run** |
| `conversation(id)` | Conversation id used to load and save memory |
| `without_memory()` | Same bypass, without supplying manual history |
| `tool_concurrency(n)` | Execute up to `n` tool calls from one model turn at once |
| `tool_context(ToolContext)` | Runtime-only values for tools; invisible to the model |
| `using_model(handle)` / `using_model_value(model)` | Override the model for this run |
| `preamble(..)`, `document(..)`, `documents(..)`, `temperature(..)`, `max_tokens(..)`, `tool_choice(..)` | Per-run overrides |
| `without_preamble()`, `without_temperature()`, `without_max_tokens()`, `without_tool_choice()`, `without_additional_params()` | Remove what the agent configured (no `without_document`) |
| `add_hook(H)` | Append a hook after the agent's default hooks |
| `run()` / `stream()` | Drive the loop, blocking or incrementally |

### Tool concurrency

```rust
let response = agent
    .runner("Check inventory, shipping, and pricing.")
    .max_turns(3)
    .tool_concurrency(3)
    .run()
    .await?;
```

Final message history is still persisted in tool-call order. With `tool_concurrency > 1`,
per-tool side effects — logs, spans, hook callbacks — may interleave in completion order,
so make them concurrency-safe.

### Model selection

`using_model(..)` sets the run's default candidate but does **not** suppress registered
model-selection hooks, which may replace it before each model call including retries.
Selections chain and the last one wins, so when a run must always use one model, append an
unconditional selecting hook last in the stack.

## Writing a Hook

`AgentHook` has no required methods — implement only the events you care about.

```rust,verify
use rig::agent::{AgentHook, HookContext, ObservationAction, ToolCall, ToolCallAction,
                 ToolResultAction, ToolResultEvent};

struct ToolAudit;

impl AgentHook for ToolAudit {
    async fn on_tool_call(&self, ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        tracing::info!(
            turn = ctx.turn(),
            tool = event.tool_name,
            args = event.args,
            "tool call"
        );
        ToolCallAction::run()
    }

    async fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        tracing::info!(tool = event.tool_name, "tool returned");
        ToolResultAction::keep()
    }
}
```

### Hook methods and their actions

| Method | Event type | Returns |
|---|---|---|
| `on_model_select` | `ModelSelection<'_>` | `ModelSelectionAction` (synchronous — no I/O) |
| `on_completion_call` | `CompletionCallEvent<'_>` | `CompletionCallAction` |
| `on_completion_response` | `CompletionResponseEvent<'_>` | `ObservationAction` |
| `on_model_turn_finished` | `ModelTurnFinished<'_>` | `ModelTurnAction` |
| `on_invalid_tool_call` | `&InvalidToolCallContext` | `Option<InvalidToolCallAction>` |
| `on_tool_call` | `ToolCall<'_>` | `ToolCallAction` |
| `on_tool_result` | `ToolResultEvent<'_>` | `ToolResultAction` |
| `on_text_delta` | `TextDelta<'_>` | `ObservationAction` |
| `on_reasoning_delta` | `ReasoningDelta<'_>` | `ObservationAction` |
| `on_tool_call_delta` | `ToolCallDelta<'_>` | `ObservationAction` |
| `on_stream_response_finish` | `StreamResponseFinish<'_>` | `ObservationAction` |
| `observes(StepEventKind) -> bool` | — | Skip work for high-frequency events |

The action types:

```rust
enum ToolCallAction   { Run, Rewrite(Value), Skip(String), Stop(String) }
enum ToolResultAction { Keep, Rewrite(ToolOutput), Stop(String) }
enum ObservationAction { Continue, Stop(String) }
enum CompletionCallAction { Continue, Patch(RequestPatch), Stop(String) }
enum InvalidToolCallAction { Fail, Retry { feedback }, Repair { tool_name }, Skip { reason }, Stop { reason } }
```

Construct them through the helpers rather than the variants:

| Action | Constructors |
|---|---|
| `ToolCallAction` | `run()`, `rewrite(args)`, `try_rewrite(&value)` (serializes for you), `skip(reason)`, `stop(reason)` |
| `ToolResultAction` | `keep()`, `rewrite(string)`, `rewrite_output(ToolOutput)`, `stop(reason)` |
| `ObservationAction` | `continue_run()`, `stop(reason)` |
| `CompletionCallAction` | `continue_run()`, `patch(RequestPatch)`, `stop(reason)` |
| `ModelTurnAction` | `continue_run()`, `repeat()`, `retry_with_feedback(text)`, `stop(reason)` |
| `InvalidToolCallAction` | `fail()`, `retry(feedback)`, `repair(tool_name)`, `skip(reason)`, `stop(reason)` |

There is no `Default` on these — the "do nothing" case is `continue_run()` / `run()` /
`keep()` depending on the event.

`ToolResultAction::rewrite` replaces only what the model sees and what content telemetry
records — the tool's raw structured result is unchanged.

### Useful event fields

```rust
pub struct ToolCall<'a> {
    pub tool_name: &'a str,
    pub tool_call_id: Option<&'a str>,   // provider's id, else rig's minted handle
    pub internal_call_id: &'a str,       // rig correlation id
    pub args: &'a str,                   // effective JSON args, including earlier rewrites
}

pub struct ToolResultEvent<'a> {
    pub tool_name: &'a str,
    pub tool_call_id: Option<&'a str>,
    pub internal_call_id: &'a str,
    pub args: &'a str,
    pub presentation: &'a ToolOutput,    // running presentation, including earlier rewrites
    pub raw_result: &'a ToolResult,      // immutable raw execution result
    pub tool_context: &'a ToolContext,   // inbound values plus result metadata
}
```

`HookContext` gives run-scoped information: `run_id()`, `turn()` (one-based model-call
index), `is_streaming()`, `agent_name()`, and `scratchpad()` — a shared scratchpad for
carrying state between hook invocations in one run.

## Where Hooks Attach

```rust
// Every request from this agent
let agent = client.agent(MODEL).add_hook(ToolAudit).build();

// One request
let answer = agent.prompt("...").add_hook(ToolAudit).await?;

// One explicit run
let response = agent.runner("...").add_hook(ToolAudit).run().await?;
```

Agent-level hooks run first; per-request and per-run hooks are appended after them.

## Hook Composition

Hooks live in a `HookStack` and run in registration order. How results combine is
**event-dependent**:

- Model selections and `on_tool_call` / `on_tool_result` rewrites **chain** — a later hook
  sees the earlier hook's rewritten arguments or presentation.
- `on_completion_call` request patches **accumulate and merge** in registration order.
- Model-turn steering, observe-only events, and invalid-call recovery use
  **first-non-continue wins**: the first hook that returns something other than the
  continue action decides, and later hooks are not consulted for that event.

When several rules must combine into one decision, compose them inside a single hook rather
than relying on stack order — otherwise the first one to return a non-continue action
silently decides for the rest.

## Guardrails and Approvals

```rust,verify
use rig::agent::{AgentHook, HookContext, ToolCall, ToolCallAction};

struct TransferPolicy {
    max_auto_transfer: u64,
}

impl AgentHook for TransferPolicy {
    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        if event.tool_name != "transfer_funds" {
            return ToolCallAction::run();
        }

        let amount = serde_json::from_str::<serde_json::Value>(event.args)
            .ok()
            .and_then(|v| v.get("amount").and_then(serde_json::Value::as_u64));

        match amount {
            Some(n) if n <= self.max_auto_transfer => ToolCallAction::run(),
            Some(n) => ToolCallAction::skip(format!(
                "denied by policy: {n} exceeds the {} automatic transfer limit",
                self.max_auto_transfer
            )),
            None => ToolCallAction::skip("denied by policy: missing transfer amount"),
        }
    }
}
```

`Skip` returns the reason to the model as the tool result, so the model can explain the
refusal or take a different path. `Stop` ends the run.

**This is a guardrail, not a security boundary.** A hook is application-level policy that
runs in the same process as the agent. Enforce real authorization inside the tool
implementation and the downstream service as well.

## Request Patches

`on_completion_call` can patch one turn of the request without mutating the agent — force
a search on the first turn, drop the temperature for a critical step, or shrink the
advertised tool list.

```rust,verify
use rig::agent::{AgentHook, CompletionCallAction, CompletionCallEvent, HookContext,
                 RequestPatch};
use rig::message::ToolChoice;

struct ForceSearchFirst;

impl AgentHook for ForceSearchFirst {
    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        if ctx.turn() != 1 {
            return CompletionCallAction::continue_run();
        }

        CompletionCallAction::patch(
            RequestPatch::new()
                .active_tools(["search_web"])
                .tool_choice(ToolChoice::Specific {
                    function_names: vec!["search_web".to_string()],
                })
                .temperature(0.0),
        )
    }
}
```

`RequestPatch` builders: `preamble`, `context`, `extra_context`, `history`,
`active_tools`, `tool_choice`, `temperature`, `max_tokens`, `additional_params`.

Patches are per-turn and non-sticky. If you narrow `active_tools`, make sure any
`tool_choice` still names a tool that is advertised — otherwise the model's only legal move
is a call it cannot make.

## Invalid Tool Calls

An invalid tool call is a name that is unknown, not advertised for the turn, or disallowed
by the active `ToolChoice`. By default Rig fails fast with
`PromptError::UnknownToolCall`. A hook can opt into recovery.

```rust,verify
use rig::agent::{AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext};

struct RepairDefaultApi;

impl AgentHook for RepairDefaultApi {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        // Some providers emit a placeholder name like `default_api` when the model
        // means "call the obvious tool". Repair it rather than failing the run.
        if event.tool_name == "default_api" {
            return Some(InvalidToolCallAction::repair("search_web"));
        }
        if event.available_tools.is_empty() {
            return None; // nothing useful to suggest; let a later hook or the default decide
        }
        Some(InvalidToolCallAction::retry(format!(
            "Use one of these tools: {:?}",
            event.available_tools
        )))
    }
}
```

Recovery options:

- `Fail` — preserve fail-fast behavior.
- `Retry { feedback }` — append corrective feedback and ask the model again. Bound it with
  `max_invalid_tool_call_retries(n)`; each retry also consumes normal turn budget.
- `Repair { tool_name }` — rewrite only the tool name, then revalidate it.
- `Skip { reason }` — record a synthetic tool result without executing anything.
- `Stop { reason }` — end the run.

Returning `None` leaves the decision to a later hook. If every hook returns `None`, Rig
keeps its fail-fast default.

`InvalidToolCallContext` carries `tool_name`, `tool_call_id`, `internal_call_id`, `args`,
`available_tools`, `allowed_tools`, `tool_choice`, `chat_history`, and `is_streaming` —
enough to build precise corrective feedback rather than a generic retry.

## High-Frequency Events

Text and tool-call deltas fire constantly on streaming runs. Override `observes` so Rig can
skip dispatch entirely when no hook cares:

```rust,verify
use rig::agent::{AgentHook, HookContext, StepEventKind, ToolCall, ToolCallAction};

struct ToolOnlyHook;

impl AgentHook for ToolOnlyHook {
    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(kind, StepEventKind::ToolCall | StepEventKind::ToolResult)
    }

    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        println!("tool: {}", event.tool_name);
        ToolCallAction::run()
    }
}
```

`observes` is an optimization, not a filter you can rely on for correctness: a sibling
hook interested in the same event kind can still cause it to be dispatched to you. Leave
your unimplemented events at their default continue action rather than assuming `observes`
will spare you.

Note the naming: the *events* are per-method types, but `observes` takes a
`StepEventKind` — the one piece of the unified `StepEvent` vocabulary that shipped.

## Best Practices

- **Keep hooks lightweight.** They are awaited inline; a slow hook delays the next model or
  tool step. Offload network writes and audit persistence to background tasks.
- **Make hook state concurrency-safe** when using `tool_concurrency > 1`.
- **Prefer one composed policy hook** when several rules must produce a single decision.
- **Do not treat hooks as authorization.** See the guardrail note above.
- **Model selection must not block.** `on_model_select` is synchronous by design; it may
  read and write the run scratchpad but must not perform I/O.

## `AgentRunner` vs `AgentRun`

Use `AgentRunner` when you still want Rig to do the I/O: send requests, execute tools,
apply memory, emit spans, run hooks. Use `AgentRun` when you want a serializable state
machine and will supply the I/O yourself — the right tool for durable human-in-the-loop
systems where you serialize the run while tools are pending, wait for an approval in
another service, then deserialize and feed results back. `AgentRun` has no hooks, because
hooks are async side-effecting driver code and live at the runner layer.

Back to the reference index in SKILL.md.
