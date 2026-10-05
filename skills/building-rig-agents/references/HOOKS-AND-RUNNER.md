# Hooks and Runner

Read this file when the user wants per-run controls, audit logging, approval flows,
guardrails, request shaping, invalid-tool-call recovery, or concurrent tool execution.

Verified against `rig` 0.43.0. The `on_event(StepEvent) -> Flow` hook API documented on
rig.rs is the 0.40 one, **replaced in 0.41**; 0.43 uses per-event methods with per-event action types. See
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

`agent.prompt(..)` returns the `AgentRunner` itself: configure it, then `.await` it (or call
`run()`) for the blocking surface, or call `stream()` for the streaming one.

## Basic Runner Usage

```rust
use rig::prelude::*;
use rig::providers::openai::{self, OpenAI};

let agent = AgentBuilder::new(OpenAI::from_env()?.completion(MODEL))
    .preamble("You are a helpful assistant.")
    .build();

let response = agent
    .prompt("Check inventory, shipping, and pricing, then summarize.")
    .max_turns(5)
    .tool_concurrency(3)
    .run()
    .await?;

println!("answer: {}", response.output);
println!("model calls: {}", response.completion_calls.len());
println!("tokens: {:?}", response.usage.total_tokens);
```


## Per-Run Controls

| Method | Effect |
|---|---|
| `max_turns(n)` | Total model-call budget, including the initial call and every retry |
| `max_invalid_tool_call_retries(n)` | Retry budget for invalid-tool-call recovery, `0` by default; retries also consume the normal turn budget |
| `unhandled_invalid_tool_call(UnhandledInvalidToolCall::Ignore)` | Drop an invalid call no hook resolves and continue the turn, instead of failing (`rig::run::UnhandledInvalidToolCall`) |
| `history(iter)` | Explicit history — **bypasses conversation memory for this run** |
| `conversation(id)` | Conversation id used to load and save memory |
| `without_memory()` | Same bypass, without supplying manual history |
| `tool_concurrency(n)` | Execute up to `n` tool calls from one model turn at once |
| `tool_context(ToolContext)` | Runtime-only values for tools; invisible to the model |
| `using_model(label)` / `using_model_value(model)` | Override the model for this run |
| `preamble(..)`, `document(..)`, `documents(..)`, `temperature(..)`, `max_tokens(..)`, `tool_choice(..)` | Per-run overrides |
| `without_preamble()`, `without_temperature()`, `without_max_tokens()`, `without_tool_choice()`, `without_additional_params()` | Remove what the agent configured (no `without_document`) |
| `add_hook(H)` | Append a hook after the agent's default hooks |
| `run()` / `stream()` | Drive the loop, blocking or incrementally |

### Tool concurrency

```rust
let response = agent
    .prompt("Check inventory, shipping, and pricing.")
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
use rig::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext, OutcomeAction,
                 OutcomeEvent};

struct ToolAudit;

impl AgentHook for ToolAudit {
    /// Every effect on the agent's bus passes here; only a tool call has a tool name.
    async fn on_dispatch(&self, ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        if let Some(tool) = event.tool_name() {
            tracing::info!(turn = ctx.turn(), tool, args = event.tool_args(), "tool call");
        }
        DispatchAction::proceed()
    }

    async fn on_outcome(&self, _ctx: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        if let Some(tool) = event.tool_name() {
            tracing::info!(tool, "tool returned");
        }
        OutcomeAction::proceed()
    }
}
```

### Hook methods and their actions

| Method | Event type | Returns |
|---|---|---|
| `on_run_start` | `RunStart<'_>` | `RunStartAction` — once, before the first model call: keep, rewrite or stop the prompt |
| `on_run_settled` | `RunSettled<'_>` | `()` — observes the final response or error |
| `on_model_select` | `ModelSelection<'_>` | `ModelSelectionAction` (synchronous — no I/O) |
| `on_completion_call` | `CompletionCallEvent<'_>` | `CompletionCallAction` |
| `on_model_turn_finished` | `ModelTurnFinished<'_>` | `ModelTurnAction` |
| `on_invalid_tool_call` | `&InvalidToolCallContext` | `Option<InvalidToolCallAction>` |
| `on_dispatch` | `DispatchEvent<'_>` | `DispatchAction` — before every tool call and completion; memory, retrieval, embed, rerank and custom effects only when `observes` opts in |
| `on_outcome` | `OutcomeEvent<'_>` | `OutcomeAction` — after it, with the tool result or the model reply |
| `on_text_delta` | `TextDelta<'_>` | `ObservationAction` |
| `on_reasoning_delta` | `ReasoningDelta<'_>` | `ObservationAction` |
| `on_tool_call_delta` | `ToolCallDelta<'_>` | `ObservationAction` |
| `observes(StepEventKind) -> bool` | — | Skip work for high-frequency events |

The action types:

```rust
enum DispatchAction { Proceed, Patch(EffectKind), Deny(ErrorReport) }
enum OutcomeAction  { Proceed, Replace(Result<Outcome, ErrorReport>) }
enum ObservationAction { Continue, Stop(String) }
enum CompletionCallAction { Continue, Patch(RequestPatch), Stop(String) }
enum InvalidToolCallAction { Fail, Retry { feedback }, Repair { tool_name }, Skip { reason }, Stop { reason } }
```

Construct them through the helpers rather than the variants:

| Action | Constructors |
|---|---|
| `DispatchAction` | `proceed()`, `rewrite_tool_args(kind, args)`, `try_rewrite_tool_args(kind, &value)` (serializes for you), `skip(reason)`, `stop(reason)` |
| `OutcomeAction` | `proceed()`, `rewrite_tool_result(&event, string)`, `rewrite_tool_output(&event, ToolOutput)`, `stop(reason)` |
| `ObservationAction` | `continue_run()`, `stop(reason)` |
| `RunStartAction` | `continue_run()`, `rewrite(prompt)`, `stop(reason)` |
| `CompletionCallAction` | `continue_run()`, `patch(RequestPatch)`, `stop(reason)` |
| `ModelTurnAction` | `continue_run()`, `repeat()`, `retry_with_feedback(text)`, `stop(reason)` |
| `InvalidToolCallAction` | `fail()`, `retry(feedback)`, `repair(tool_name)`, `skip(reason)`, `stop(reason)` |

There is no `Default` on these — the "do nothing" case is `continue_run()` or `proceed()`
depending on the event. `DispatchAction::skip` on a tool call is a result the model sees;
on a completion it fails the run, so gate on `event.tool_name()` first.

`OutcomeAction::rewrite_tool_result` replaces only what the model sees, keeping the result's
status and the dispatch context.

### Useful event fields

```rust
pub struct DispatchEvent<'a> {
    pub id: EffectId,                    // correlates the dispatch with its outcome
    pub kind: &'a EffectKind,            // the effect, including earlier hooks' patches
    pub turn: usize,
    pub call_id: Option<&'a CallId>,     // the model's call, for a tool call
    pub context: Option<&'a ToolContext>,
}

pub struct OutcomeEvent<'a> {
    pub id: EffectId,
    pub kind: &'a EffectKind,
    pub outcome: &'a Result<Outcome, ErrorReport>, // including earlier hooks' replacements
    pub turn: usize,
    pub call_id: Option<&'a CallId>,
    pub context: Option<&'a ToolContext>, // inbound values plus result metadata
}
```

Read them through the accessors: `tool_name()`, `tool_args()` and `tool_context()` on both,
`tool_result()` and `completion()` on an outcome; each is `None` for any other effect.

`HookContext` gives run-scoped information: `run_id()`, `turn()` (one-based model-call
index), `is_streaming()`, `agent_name()`, and `scratchpad()` — a shared scratchpad for
carrying state between hook invocations in one run.

## Where Hooks Attach

```rust
// Every request from this agent
let agent = AgentBuilder::new(client.completion(MODEL)).add_hook(ToolAudit).build();

// One run
let answer = agent.prompt("...").add_hook(ToolAudit).max_turns(5).await?;
```

Agent-level hooks run first; per-run hooks are appended after them.

## Hook Composition

Hooks live in a `HookStack` and run in registration order. How results combine is
**event-dependent**:

- Model selections, `on_run_start` rewrites, `on_dispatch` patches and `on_outcome`
  replacements **chain** — a later hook sees the earlier hook's rewritten prompt, patched
  arguments or replaced result; a denial or stop ends the chain.
- `on_run_settled` reaches every hook.
- `on_completion_call` request patches **accumulate and merge** in registration order.
- Model-turn steering, observe-only events, and invalid-call recovery use
  **first-non-continue wins**: the first hook that returns something other than the
  continue action decides, and later hooks are not consulted for that event.

When several rules must combine into one decision, compose them inside a single hook rather
than relying on stack order — otherwise the first one to return a non-continue action
silently decides for the rest.

## Guardrails and Approvals

```rust,verify
use rig::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};

struct TransferPolicy {
    max_auto_transfer: u64,
}

impl AgentHook for TransferPolicy {
    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        if event.tool_name() != Some("transfer_funds") {
            return DispatchAction::proceed();
        }

        let amount = event
            .tool_args()
            .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
            .and_then(|v| v.get("amount").and_then(serde_json::Value::as_u64));

        match amount {
            Some(n) if n <= self.max_auto_transfer => DispatchAction::proceed(),
            Some(n) => DispatchAction::skip(format!(
                "denied by policy: {n} exceeds the {} automatic transfer limit",
                self.max_auto_transfer
            )),
            None => DispatchAction::skip("denied by policy: missing transfer amount"),
        }
    }
}
```

`skip` returns the reason to the model as the tool result, so the model can explain the
refusal or take a different path. `stop` ends the run.

**This is a guardrail, not a security boundary.** A hook is application-level policy that
runs in the same process as the agent. Enforce real authorization inside the tool
implementation and the downstream service as well.

## Request Patches

`on_completion_call` can patch one turn of the request without mutating the agent — forbid
tool calls on the last turn of the budget, or raise `max_tokens` for one long answer.

```rust,verify
use rig::agent::{AgentHook, CompletionCallAction, CompletionCallEvent, HookContext,
                 RequestPatch};
use rig::message::ToolChoice;

/// With `.max_turns(3)` the third model call is the last one, and a tool call there ends
/// the run in `MaxTurnsError`.
struct AnswerOnLastTurn;

impl AgentHook for AnswerOnLastTurn {
    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        _event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        if ctx.turn() < 3 {
            return CompletionCallAction::continue_run();
        }

        CompletionCallAction::patch(RequestPatch::new().tool_choice(ToolChoice::None))
    }
}
```

`RequestPatch` builders: `preamble`, `context`, `extra_context`, `history`,
`active_tools`, `tool_choice`, `temperature`, `max_tokens`, `additional_params`.

On Claude Opus 5.5 and Fable 5.1 every thinking block is bound to the `system` prompt, the
tool set and the messages before it. A patch to `preamble`, `active_tools` or `history`
changes that prefix, so the next request that replays the block gets a 400 wherever the API
enforces the check, which includes every account created from 31 August 2026.
`tool_choice`, `max_tokens` and `additional_params` sit outside the prefix. A forced
`ToolChoice::Required` or `Specific` is a 400 on both, and on Sonnet 5.5, regardless.

Patches are per-turn and non-sticky. If you narrow `active_tools`, make sure any
`tool_choice` still names a tool that is advertised — otherwise the model's only legal move
is a call it cannot make.

## Invalid Tool Calls

An invalid tool call is a name that is unknown, not advertised for the turn, or disallowed
by the active `ToolChoice` — or, since 0.43 and only in a streamed run, a call whose
arguments are not JSON; `event.reason` tells the two apart. By default Rig fails fast with
`PromptError::UnknownToolCall`, or for malformed arguments a `PromptError::Report` of kind
`Response`, which is also how a blocking run fails on them without consulting any hook. A
hook can opt into recovery.

```rust,verify
use rig::agent::{
    AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext, InvalidToolCallReason,
};

struct RepairDefaultApi;

impl AgentHook for RepairDefaultApi {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        if !matches!(event.reason, InvalidToolCallReason::UnknownTool) {
            return None; // malformed arguments: a repaired name would not fix them
        }
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
- `Retry { feedback }` — append corrective feedback and ask the model again. The budget,
  `max_invalid_tool_call_retries(n)`, defaults to `0`, so a `Retry` fails like `Fail` until
  you set it; each retry also consumes normal turn budget.
- `Repair { tool_name }` — rewrite only the tool name, then revalidate it.
- `Skip { reason }` — record a synthetic tool result without executing anything.
- `Stop { reason }` — end the run.

Returning `None` leaves the decision to a later hook. If every hook returns `None`, Rig
keeps its fail-fast default.

`InvalidToolCallContext` carries `tool_name`, `tool_call_id`, `args`, `available_tools`,
`allowed_tools`, `tool_choice`, `chat_history`, `is_streaming` and `reason` —
enough to build precise corrective feedback rather than a generic retry.

## High-Frequency Events

Text and reasoning deltas fire constantly on streaming runs. Override `observes` so Rig can
skip dispatch entirely when no hook cares:

```rust,verify
use rig::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext, StepEventKind};

struct ToolOnlyHook;

impl AgentHook for ToolOnlyHook {
    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(kind, StepEventKind::ToolDispatch)
    }

    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        if let Some(tool) = event.tool_name() {
            println!("tool: {tool}");
        }
        DispatchAction::proceed()
    }
}
```

For the delta events `observes` is an optimization, not a filter you can rely on for
correctness: a sibling hook interested in the same event kind can still cause it to be
dispatched to you. Leave your unimplemented events at their default continue action rather
than assuming `observes` will spare you. For `on_dispatch` and `on_outcome` it *is* a
filter: a hook that answers `false` for `ToolDispatch` is never called for a tool call and
cannot gate one, so keep the dispatch kinds a hook means to gate.

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
hooks are async side-effecting driver code and live at the runner layer. Since 0.43,
`agent.resume(run)` turns a deserialized `AgentRun` back into an `AgentRunner` that continues
under the agent's hooks and tools; the run keeps its own history and turn budget, and pending
tool calls execute again.

Back to the reference index in SKILL.md.
