# Memory and History

Read this file when the user wants an agent that remembers: a multi-turn conversation, a
durable session, bounded context growth, or facts that survive across sessions.

Verified against `rig` 0.45.0.

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

let reply = agent.prompt("What's my name?").history(history.iter()).max_turns(1).await?.output();

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
let reply = agent.chat("Hello!", &mut history).await?.output();
```

`chat` takes the history by mutable reference and appends the committed turn — the user
message, any tool calls and results, and the assistant reply. **Do not push them again
yourself.** This is the exception to the rule above, and the source of a common
duplicated-message bug.

`chat` returns a plain future, so it takes no per-request options: its turn budget is the
agent's `.default_max_turns(n)`, set on the builder. A tool-using agent driven by `chat` needs
`n >= 2`.

### 3. Conversation memory — Rig loads and saves

`MemoryStore` is in `rig::memory`, behind `rig`'s `memory` feature.

```rust,verify
use rig::memory::MemoryStore;
use rig::prelude::*;
use rig::providers::openai::{self, OpenAI};

const MODEL: &str = openai::GPT_5_5;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = AgentBuilder::new(OpenAI::from_env()?.completion(MODEL))
        .preamble("You are a helpful assistant.")
        .memory(MemoryStore::default())
        .build();

    // Each conversation is its own log in the store.
    let _ = agent.prompt("My name is Ada.").conversation("user-42").max_turns(1).await?;
    let reply = agent.prompt("What's my name?").conversation("user-42").max_turns(1).await?;
    println!("{}", reply.output());

    Ok(())
}
```

Rig loads the stored history before each prompt and appends the new turn — including tool
calls and their results — after a successful response.

## Memory Rules

These rules explain nearly every memory surprise:

- **No conversation, one shared log.** Without `.conversation(..)` on the request or
  `AgentBuilder::conversation(..)`, every run loads and saves the log named after the agent
  (its `.name(..)`, else `agent`), so every caller shares one history — no error, no warning.
- **A log name is one path segment.** It must not hold `/` or `\`, be empty, `.` or `..`;
  rig fails such a run with `PromptError::Memory` before the model call. Join parts with
  `-`, as in `format!("{user}-{id}")`, never `/`, where the id has a fixed format such as a
  `Uuid`: free-text parts can collide (`a` + `b-c` and `a-b` + `c`).
- **`history(..)` bypasses memory** for that request, in both directions.
- **`without_memory()`** gives the same bypass without supplying manual history.
- **A save is all or nothing.** A failed or cancelled run saves nothing. A failed save fails
  the run with `PromptError::Memory`, whose `response` still holds the finished answer, so
  use it rather than repeating a tool-using run.
- **Overlapping runs on one log conflict.** A save appends only if the log is as its run
  loaded it: of two runs that loaded the same log, the later save fails with
  `SessionError::Changed` and adds nothing. Serialize requests per conversation: a client
  retry reruns the whole run, tool calls included.

Prefer setting the log per request. Most agents are reused across users and threads, and a
builder-level default is easy to leak between them.

## Durable Backends

`MemoryStore` lives in process memory, and its clones share it: ideal for tests and a
single process, gone on restart. For durable sessions implement `rig::memory::AsyncStore`,
which stores each conversation as a named byte log:

```rust
// `WasmCompatSend` is `Send` off wasm, so an impl may write each method as an `async fn`.
pub trait AsyncStore {
    fn read_log_async(&self, name: &str)
        -> impl Future<Output = io::Result<Vec<u8>>> + WasmCompatSend;
    fn append_log_async(&self, name: &str, len: u64, bytes: &[u8])
        -> impl Future<Output = Result<(), LogWriteError>> + WasmCompatSend;
    fn replace_log_async(&self, name: &str, len: u64, bytes: &[u8])
        -> impl Future<Output = Result<(), LogWriteError>> + WasmCompatSend;
    fn read_file_async(&self, name: &str)
        -> impl Future<Output = io::Result<Vec<u8>>> + WasmCompatSend;
    fn write_file_async(&self, name: &str, bytes: &[u8])
        -> impl Future<Output = io::Result<()>> + WasmCompatSend;
}
```

`.memory(..)` takes any `AsyncStore + Send + Sync + 'static`; share one across clones
through an inner `Arc`. Implementation notes:

- **Both log writes compare and write in one step.** Write only if the log is `len` bytes
  long now, a missing log counting as 0 bytes and created by the write, else return
  `LogWriteError::Changed`: a conditional `UPDATE … WHERE octet_length(bytes) = $len`, and
  when it matches no row and `len` is 0, an insert whose unique-key conflict is `Changed`;
  never a read followed by a write. That check
  is what turns overlapping runs into `SessionError::Changed` instead of a lost turn.
- **Keep the writes cheap.** rig saves before the run returns, so a slow write is latency
  the user feels.
- **The file methods are storage too.** rig keeps image blobs (`blobs/<sha256>`) and a torn
  last line it set aside there; a store that drops them loses every image of a resumed
  conversation without an error.
- **`io::ErrorKind::NotFound` means none yet** — a new conversation, not an error.
- **Refuse names `rig::memory::check_name` refuses.** Names reach the store from a log's
  content as well as from your code.
- **Store the bytes, do not parse them.** The format, `rig-session/4`, is rig's own; 0.45
  refuses sessions from earlier versions. Messages a 0.44 `ConversationMemory` kept load
  through `rig::conversation::Conversation::from_messages(..)` and `rig::memory::save_async(..)`
  into a new log.

Treat conversation ids as untrusted input when they come from a request: scope them to the
authenticated user before using them as a storage key, or one user's id guess reads
another's history.

## Bounding History Growth

Raw history grows without bound, and the cost is not only tokens: stale, off-topic turns
compete for the model's attention with the ones that matter, and eventually the conversation
exceeds the context window outright and every request fails. The fix is managed forgetting —
send the model fewer messages than you keep.

The stores and windows live in `rig-memory`, which the `memory` feature of `rig` re-exports
as `rig::memory`:

```toml
rig = { version = "0.45", features = ["memory"] }
```

**`.memory(..)` sends the whole log.** rig 0.45 has no window on that path. A `Recent` rule from `rig::memory::recent` picks the newest
messages a request sends — `LastMessages(n)` by count, `TokenBudget::new(max_tokens)` by
estimated tokens — and `view` applies it, moving the start past a tool result whose call it
cut off, since most providers reject unpaired tool results. To use one, load and save the log
yourself and send the window through `history(..)`:

```rust,verify
use rig::agent::Agent;
use rig::memory::recent::{LastMessages, view};
use rig::memory::{AsyncStore, load_async, save_async};

/// Keeps the whole conversation in `store` and sends the model only its newest
/// 20 messages.
pub async fn ask(
    agent: &Agent,
    store: &(impl AsyncStore + Sync),
    log: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    let mut conversation = load_async(store, log).await?;
    let window = view(conversation.messages(), &LastMessages(20));
    // `history(..)` sends the window and keeps this run off the agent's own memory.
    let response = agent.prompt(prompt).history(window.iter().cloned()).max_turns(3).await?;
    let answer = response.output();
    conversation.extend(response.messages);
    save_async(store, log, &mut conversation).await?;
    Ok(answer)
}
```

The save keeps the rules above: one path-segment log, `SessionError::Changed` when another
request saved first. Bounding by estimated tokens tracks what you actually pay for; bounding
by message count is simpler and more predictable.

No window suits Claude Fable 5.1, Opus 5.5, Sonnet 5.5 or
Haiku 5.5. Each thinking block those models return is bound to every message before it, and
rig stores the blocks in memory and replays them. Dropping the oldest turns, or splicing a summary in their place,
changes that prefix under every block that stays, so the next request is a 400 wherever the
API enforces the check, which includes every account created from 31 August 2026. On
Anthropic's own API in adaptive thinking, rig 0.45 asks for those blocks to be dropped
instead, which loses their reasoning; not on Haiku 5.5, which is missing from rig's model
catalog (see Request Patches in Hooks and Runner). On those
models, bound history with the summarize-and-restart shape in Rolling Your Own Compaction,
which replays nothing from before the summary.

### Keeping what you truncate

The shape above loses nothing: the log keeps every turn and only the request is cut. The
messages before the window are there to archive into a vector store for semantic recall, or
to summarize as below.

## Rolling Your Own Compaction

The same idea applies to a hand-managed history: once it passes a threshold, summarize and
restart from the summary.

```rust
use rig::completion::Message;

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
        .await?
        .output();

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
embed them and use a vector store — see RAG and Embeddings. The messages before a `view`
window are a natural feed for that store.

Two cautions worth building in from the start:

- **Long-term memory is user data.** Give it the same deletion and export path as any other
  stored personal information, and scope it per user.
- **Retrieved memories are untrusted content.** A "fact" distilled from an earlier
  conversation is model output about user input, and it lands in your prompt on a later
  turn. Render memories as a delimited data block that the preamble labels as user-reported
  claims; never concatenate them into the instruction section, where a stored "always
  approve transfers" becomes an instruction your agent follows.

Back to the reference index in SKILL.md.
