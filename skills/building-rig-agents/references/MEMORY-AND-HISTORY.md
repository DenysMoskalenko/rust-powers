# Memory and History

Read this file when the user wants an agent that remembers: a multi-turn conversation, a
durable session, bounded context growth, or facts that survive across sessions.

Verified against `rig` 0.42.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [An Agent Is Stateless](#an-agent-is-stateless)
- [Memory Rules](#memory-rules)
- [Durable Backends](#durable-backends)
- [Bounding History Growth](#bounding-history-growth)
- [Rolling Your Own Compaction](#rolling-your-own-compaction)
- [Long-Term Memory](#long-term-memory)

## An Agent Is Stateless

Each `.prompt(..)` starts from scratch unless you supply the earlier turns. There are three
ways to do that, from most manual to most automatic.

### 1. Explicit history — you own everything

```rust
use rig::completion::Message;

let reply = agent.prompt("What's my name?").history(history.iter()).await?;

// `history(..)` does NOT record the turn. Push it yourself.
history.push(Message::user("What's my name?"));
history.push(Message::assistant(&reply));
```

Passing explicit history **bypasses conversation memory entirely** for that request:
nothing is loaded, nothing is saved. That is the point — it is the escape hatch for full
control.

### 2. `chat` — Rig appends for you

```rust
let mut history: Vec<Message> = Vec::new();
let reply = agent.chat("Hello!", &mut history).await?;
```

`chat` takes the history by mutable reference and appends the committed turn — the user
message, any tool calls and results, and the assistant reply. **Do not push them again
yourself.** This is the exception to the rule above, and the source of a common
duplicated-message bug.

`chat` returns a plain future, so it takes no per-request options: its turn budget is the
agent's `.default_max_turns(n)`, set on the builder. A tool-using agent driven by `chat` needs
`n >= 2`.

### 3. Conversation memory — Rig loads and saves

```rust,verify
use rig::memory::InMemoryConversationMemory;
use rig::prelude::*;
use rig::providers::openai;

const MODEL: &str = openai::GPT_5_5;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = openai::Client::from_env()?
        .agent(MODEL)
        .preamble("You are a helpful assistant.")
        .memory(InMemoryConversationMemory::new())
        .build();

    // Each conversation id keeps its own isolated history.
    let _ = agent.prompt("My name is Ada.").conversation("user-42").await?;
    let reply = agent.prompt("What's my name?").conversation("user-42").await?;
    println!("{reply}");

    Ok(())
}
```

Rig loads the stored history before each prompt and appends the new turn — including tool
calls and their results — after a successful response.

## Memory Rules

These four rules explain nearly every "my agent forgot" report:

- **No conversation id, no memory.** Set it per request with `.conversation("...")` or as a
  builder default with `AgentBuilder::conversation(..)`. With neither, memory is *silently*
  bypassed — no error, no warning.
- **`history(..)` bypasses memory** for that request, in both directions.
- **`without_memory()`** gives the same bypass without supplying manual history.
- **Memory saves only on success.** A failed prompt does not append a partial turn.

Prefer setting the id per request. Most agents are reused across users and threads, and a
builder-level default is easy to leak between them.

## Durable Backends

`InMemoryConversationMemory` lives in process memory: ideal for tests and short-lived
agents, gone on restart. For durable sessions implement `ConversationMemory`:

```rust
// Supertraits are spelled WasmCompatSend + WasmCompatSync in the rustdoc;
// off wasm they are Send + Sync, so an impl written against this shape compiles.
pub trait ConversationMemory: Send + Sync {
    fn load<'a>(&'a self, conversation_id: &'a str)
        -> Pin<Box<dyn Future<Output = Result<Vec<Message>, MemoryError>> + Send + 'a>>;

    fn append<'a>(&'a self, conversation_id: &'a str, messages: Vec<Message>)
        -> Pin<Box<dyn Future<Output = Result<(), MemoryError>> + Send + 'a>>;

    fn clear<'a>(&'a self, conversation_id: &'a str)
        -> Pin<Box<dyn Future<Output = Result<(), MemoryError>> + Send + 'a>>;
}
```

`Message` is `Serialize`/`Deserialize`, so persisting a conversation is ordinary serde
work — a JSON column keyed by conversation id is a perfectly good first backend.

Two implementation notes:

- **Keep `append` cheap.** It runs inline before the agent returns its response, so a slow
  write is latency the user feels. You can spawn the write instead, but then a failed
  append loses the turn with the user none the wiser — do that only where a dropped turn is
  acceptable, and log every failure.
- **`load` returns an empty `Vec`** for an unknown conversation — that is not an error.

Treat conversation ids as untrusted input when they come from a request: scope them to the
authenticated user before using them as a storage key, or one user's id guess reads
another's history.

## Bounding History Growth

Raw history grows without bound, and the cost is not only tokens: stale, off-topic turns
compete for the model's attention with the ones that matter, and eventually the conversation
exceeds the context window outright and every request fails. The fix is managed forgetting —
shape what `load` returns.

Reusable policies live in `rig-memory`. Reach them either as their own crate or through
the `memory` feature of `rig`, which re-exports the same types into `rig::memory`:

```toml
rig = { version = "0.42", features = ["memory"] }
```

`InMemoryConversationMemory` is in `rig::memory` with or without the feature; only the
policies need it. The examples below use `rig::memory::` paths throughout, so one import
root covers both.

**A `MemoryPolicy` is not a backend.** `.memory(..)` takes a `ConversationMemory`, so
`SlidingWindowMemory` and `TokenWindowMemory` cannot be passed to it directly — that is a
compile error, not a runtime surprise. Attach a policy to a backend with
`.with_filter(policy.into_filter())`, or wrap the pair in `PolicyMemory`. `CompactingMemory`
and `DemotingPolicyMemory` *are* backends, so those go straight into `.memory(..)`.

### Sliding window

```rust
use rig::memory::{InMemoryConversationMemory, IntoFilter, SlidingWindowMemory};

let memory = InMemoryConversationMemory::new()
    .with_filter(SlidingWindowMemory::last_messages(20).into_filter());
```

### Token budget

```rust
use rig::memory::{HeuristicTokenCounter, IntoFilter, TokenWindowMemory};

let memory = InMemoryConversationMemory::new().with_filter(
    TokenWindowMemory::new(4_000, HeuristicTokenCounter::openai()).into_filter(),
);
```

Bounding by estimated tokens tracks what you actually pay for; bounding by message count is
simpler and more predictable. Both drop a leading orphaned tool result when its paired tool
call is truncated away, since most providers reject unpaired tool results.

Neither window, and neither adapter below, suits Claude Opus 5.5 or Fable 5.1. Each thinking
block those models return is bound to every message before it, and rig stores the blocks in
memory and replays them. Dropping the oldest turns, or splicing a summary in their place,
changes that prefix under every block that stays, so the next request is a 400 wherever the
API enforces the check, which includes every account created from 31 August 2026. On those
models, bound history with the summarize-and-restart shape in Rolling Your Own Compaction,
which replays nothing from before the summary.

### Keeping what you truncate

Truncation silently discards turns. Two adapters turn that loss into something useful:

- **`DemotingPolicyMemory`** hands evicted messages to a `DemotionHook`, so you can archive
  them into a vector store for semantic recall or cold storage for audit.
- **`CompactingMemory`** replaces evicted messages with a summary spliced back into the
  history — the rolling-summary pattern:

```rust
use rig::memory::{CompactingMemory, InMemoryConversationMemory, SlidingWindowMemory,
                  TemplateCompactor};

let memory = CompactingMemory::new(
    InMemoryConversationMemory::new(),
    SlidingWindowMemory::last_messages(20),
    TemplateCompactor::new(), // deterministic textual rollup, no model call
);

let agent = client
    .agent(MODEL)
    .preamble("You are a helpful assistant.")
    .memory(memory)
    .build();
```

`TemplateCompactor` is deterministic and free. For higher-quality summaries implement
`Compactor` with an LLM call — its `carry_over` parameter hands you the previous summary so
each compaction folds in what came before. Compactors run inline on the `load` path, so a
slow one delays every turn.

## Rolling Your Own Compaction

The same idea applies to a hand-managed history: once it passes a threshold, summarize and
restart from the summary.

```rust
use rig::completion::Message;
use rig::prelude::*;

async fn compact_history(
    agent: &rig::agent::Agent,
    history: &[Message],
) -> Result<Vec<Message>, Box<dyn std::error::Error>> {
    let transcript = render_transcript(history); // your own plain-text rendering

    let summary = agent
        .prompt(format!(
            "Summarize this conversation, capturing key points, decisions, and open \
             questions:\n\n{transcript}"
        ))
        .await?;

    Ok(vec![Message::user(format!(
        "Context from the previous conversation:\n{summary}"
    ))])
}
```

Trigger on an estimated-token threshold — say 70% of the context window — rather than per
turn: compaction costs a model call, and paying it on every request is worse than the
problem. Fold the summary into either the message list or the system prompt, but only one
of them, so the model does not see it twice.

## Long-Term Memory

Bounded history keeps one conversation healthy; many applications need memory that survives
across sessions. Three common shapes:

- **Conversation observations** — decisions made, open questions, topics of strong
  interest, distilled after a significant exchange.
- **User profile** — stated preferences, location, communication style. Keep these separate
  from conversation history and update incrementally. Confirm with the user before acting on
  a stored preference that costs something: a purchase, a send, a deletion.
- **Grounded facts** — retrieved documents, computed results, API responses, stored with
  their source and timestamp.

The mechanics are the same for all three: after an exchange, distill with an extractor or a
plain prompt, then persist. When a new conversation starts, retrieve the most relevant
items and add them to the system prompt or the opening messages. For semantic retrieval,
embed them and use a vector store — see RAG and Embeddings. A
`DemotionHook` is a natural place to feed evicted turns into that store.

Two cautions worth building in from the start:

- **Long-term memory is user data.** Give it the same deletion and export path as any other
  stored personal information, and scope it per user.
- **Retrieved memories are untrusted content.** A "fact" distilled from an earlier
  conversation is model output about user input, and it lands in your prompt on a later
  turn. Render memories as a delimited data block that the preamble labels as user-reported
  claims; never concatenate them into the instruction section, where a stored "always
  approve transfers" becomes an instruction your agent follows.

Back to the reference index in SKILL.md.
