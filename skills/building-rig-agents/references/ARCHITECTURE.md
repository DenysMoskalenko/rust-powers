# Architecture and Decision Guide

Decision trees and the layer map for Rig. Load this file when the user is choosing between
abstractions; if they already know what they want to do, open the narrower task guide
instead — SKILL.md lists them. Each outcome is documented in full by the file that owns it,
so this one deliberately carries no API tables of its own.

## Contents

- [The Layer Stack](#the-layer-stack)
- [Decision Trees](#decision-trees)
- [Sizing Heuristics](#sizing-heuristics)
- [Where State Lives](#where-state-lives)

## The Layer Stack

```text
Your app
  │
  ├─ Agent ───────────── preamble + context + tools + memory, runs the loop
  │    ├─ AgentRunner ── one run: turn budget, tool concurrency, hooks, tool context
  │    └─ AgentRun ───── sans-IO state machine; serialize and resume the loop yourself
  │
  ├─ Extractor<T> ────── agent + submit tool, for typed extraction
  │
  ├─ CompletionModel ─── one request, full control
  ├─ EmbeddingModel ──── text → vectors
  └─ VectorStoreIndex ── similarity search
       │
    Provider clients (OpenAI, Anthropic, Gemini, Cohere, Ollama, …)
```

Everything above a provider client is provider-agnostic, so switching vendors changes one
string and one client type. Two axes explain most "which one" questions: **altitude**, how
much of the request and loop Rig runs for you, and **control**, who decides the next step —
the model in an agent loop, your code in a workflow.

## Decision Trees

### Agent or workflow?

```text
Do you know the steps in advance?
├── Yes → Does any step need a model at all?
│   ├── No  → Write a plain function
│   └── Yes → Write a workflow: sequential lets, join!, match
└── No  → Does the model need to choose tools and order at runtime?
    ├── Yes → Agent with tools
    └── No  → Single agent prompt, no tools
```

Prefer the least agentic design that works: the loop costs turns spent deliberating and
makes behavior non-reproducible, so buy it only where the model's judgment about *what to
do next* is the value.

### Which prompting interface?

```text
Need typed output?
├── Yes → Is extraction the whole job?
│   ├── Yes → Extractor<T>
│   └── No  → agent.prompt_typed::<T>(..)   (or output_schema on the builder)
└── No → Need tokens as they arrive?
    ├── Yes → agent.stream_prompt(..)
    └── No  → Carrying conversation history?
        ├── Yes, and Rig should persist it → memory + .conversation(id)
        ├── Yes, and I own the Vec         → agent.chat(prompt, &mut history)
        └── No                             → agent.prompt(..)
```

There is no `Completion` trait in 0.42 — it was removed after 0.39. Drop to
`CompletionModel::completion_request(..)` when you want full control of one request.

### Which tool form?

```text
Does it need runtime values the model must not see (auth, tenant, session)?
├── Yes → impl Tool, read them from &mut ToolContext
└── No → Does it hold state, need a custom error type, or need a schema
         the macro cannot express?
    ├── Yes, and it never needs context → impl PortableTool
    ├── Yes, otherwise                  → impl Tool
    └── No                              → #[rig::tool_macro]

Then:
  Many tools, only a few relevant per request? → retrieved_tools(n, index, toolset)
  Tools defined at runtime?                    → dynamic_tool / dynamic_tools
  Tools served by another process?             → rmcp_tools
  Several agents sharing one mutable set?      → ToolServer + tool_server_handle
```

### How should the agent get context?

```text
Is the context small, fixed, and always relevant?
├── Yes → .context("...") static documents
└── No → Is it a corpus you can embed?
    ├── Yes → .dynamic_context(n, index)
    └── No  → Is it fetched by an external call?
        └── Yes → a tool, so the model decides when to fetch
```

Static context costs tokens on every request, so anything conditional belongs in retrieval
or a tool.

### Where should this logic live?

```text
Should the model be able to choose whether it happens?
├── Yes → a tool
└── No → Does it need to inspect or change a run in flight?
    ├── Yes → a hook (on_tool_call, on_completion_call, …)
    └── No  → ordinary Rust around the call
```

A hook is the right home for cross-cutting concerns the model should not negotiate: audit,
policy, redaction, request shaping. A tool is the right home for capability.

### Which output mode?

`OutputMode::Native` when you need a hard guarantee the response matches the schema and the
provider supports it; otherwise leave `Auto`, which picks `Tool` where tools work and
`Prompted` where they do not. Both fallbacks are best-effort — validate the JSON.

### How much memory?

```text
Multi-turn conversation?
├── No  → nothing; each prompt is independent
└── Yes → Who should own the history?
    ├── You  → agent.chat(p, &mut history), or .history(..) + push it yourself
    └── Rig  → .memory(backend) + .conversation(id)
                   └── Will it outgrow the context window?
                        ├── No                       → the backend alone
                        ├── Old turns are disposable → backend + a MemoryPolicy
                        │                               (SlidingWindowMemory / TokenWindowMemory)
                        ├── The gist matters         → CompactingMemory(backend, policy, compactor)
                        └── Evicted turns are needed → DemotingPolicyMemory → vector store
```

A `MemoryPolicy` is not a backend and cannot be passed to `.memory(..)` — see
Memory and History.

### How to test it?

Your own logic — wiring, prompts, budgets, error paths — goes to `MockCompletionModel` and
friends: offline, deterministic, free. Whether the model's *answers* are good is an
extractor-based judge run as a separate paid suite, never in `cargo test`.

## Sizing Heuristics

Starting points, not laws — measure against your own workload.

| Knob | Start at | Raise when |
|---|---|---|
| `max_turns` | 3–5 for a tool-using agent | The real tool chain is longer; watch for loops first |
| Tools per request | As few as the task needs | Prefer not to; split agents or use `retrieved_tools` |
| `dynamic_context` samples | 2–5 | Answers miss information that is in the corpus |
| Chunk size | 512–1000 tokens, 10–20% overlap | Answers straddle chunk boundaries |
| `tool_concurrency` | 1 | A model turn routinely emits independent calls |
| Retry attempts | 3, exponential backoff | Rarely; add jitter and a limiter first |

## Where State Lives

Knowing which layer owns a value answers most "why did it not persist" questions.

| State | Owner | Lifetime |
|---|---|---|
| Preamble, static context, tools, default hooks | `Agent` | The agent |
| Turn budget, tool concurrency, per-run overrides, hooks | `AgentRunner` / `PromptRequest` | One run |
| Conversation history | `ConversationMemory` backend, or your `Vec<Message>` | Per conversation id |
| Values passed to tools, invisible to the model | `ToolContext` | One run's tool dispatches |
| Cross-hook state within a run | `HookContext::scratchpad()` | One run |
| Documents and embeddings | Vector store | Until you delete them |

Nothing in the agent itself is persisted, so anything that must survive a process restart
needs a durable backend: a `ConversationMemory` implementation or a persistent vector
store.

Back to the reference index in SKILL.md.
