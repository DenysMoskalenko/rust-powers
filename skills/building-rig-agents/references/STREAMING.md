# Streaming

Read this file when the user wants tokens as they arrive: a responsive UI, long-form
output, or live observation of tool activity.

Verified against `rig` 0.42.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [Stream an Agent](#stream-an-agent)
- [Streaming with History](#streaming-with-history)
- [What the Stream Yields](#what-the-stream-yields)
- [Low-Level Streaming](#low-level-streaming)
- [Trait Mirrors](#trait-mirrors)
- [Practical Notes](#practical-notes)

## Stream an Agent

```rust,verify
use futures::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::prelude::*;
use rig::providers::openai;
use rig::streaming::StreamedAssistantContent;
const MODEL: &str = openai::GPT_5_5;


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = openai::Client::from_env()?
        .agent(MODEL)
        .preamble("You are a storyteller.")
        .temperature(0.9)
        .build();

    let mut stream = agent.stream_prompt("Tell me a short story about a robot.").await;

    while let Some(item) = stream.next().await {
        match item? {
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t)) => {
                print!("{}", t.text);
            }
            MultiTurnStreamItem::FinalResponse(response) => {
                println!("\n[{} tokens]", response.usage.total_tokens);
            }
            _ => {}
        }
    }

    Ok(())
}
```

`stream_prompt` comes from `StreamingPrompt` (in the prelude) and returns a
`StreamingPromptRequest` — the same option surface as `PromptRequest`, so
`.max_turns(..)`, `.conversation(..)`, `.add_hook(..)` all chain before you await it.

For the common terminal case Rig ships a helper:

```rust
use rig::agent::stream_to_stdout;

let mut stream = agent.stream_prompt("Hello!").await;
stream_to_stdout(&mut stream).await?;
```

It prints streamed assistant text and reasoning as they arrive, and ignores tool-call
deltas, which are rarely meaningful to display raw.

## Streaming with History

```rust
use rig::streaming::StreamingChat;

let mut stream = agent.stream_chat("Continue the story", chat_history).await;
```

Note the asymmetry with blocking `chat`, which appends the committed turn to the `Vec` you
pass. A stream can be dropped half-way, so do not assume the turn was recorded because you
started one: record it yourself when you see `FinalResponse`, or attach conversation memory
and let Rig persist on success.

## What the Stream Yields

The whole agent loop flows through one stream, so you observe tool activity live, not just
text.

```rust
pub enum MultiTurnStreamItem {
    StreamAssistantItem(StreamedAssistantContent),
    ToolExecutionCommitted { tool_call: ToolCall, internal_call_id: String },
    StreamUserItem(StreamedUserContent),
    CompletionCall(CompletionCall),
    ModelTurnRetried { turn: usize },
    FinalResponse(PromptResponse),
}
```

- **`StreamAssistantItem`** — what the model emitted: text and reasoning deltas, tool-call
  deltas, and the complete tool call for each call Rig routes to execution.
- **`ToolExecutionCommitted`** — confirmation that Rig executed and committed a tool call.
  This is *not* a real-time start notification: it surfaces together with its result only
  after the whole batch settles. For live host-side start/result observation, use tool
  hooks.
- **`CompletionCall`** — per-model-call detail: usage, finish reason, provider request id.
- **`ModelTurnRetried`** — a turn was retried.
- **`FinalResponse`** — the completed run, carrying `output`, aggregated `usage`,
  `completion_calls`, and `messages`.

### `StreamedAssistantContent`

```rust
pub enum StreamedAssistantContent {
    Text(Text),
    ToolCall { tool_call: ToolCall, internal_call_id: String },
    ToolCallDelta { internal_call_id: String, content: ToolCallDeltaContent },
    Reasoning { reasoning: Reasoning, id: String },
    ReasoningDelta { id: String, provider_id: Option<String>, reasoning: String },
    Final(StreamFinal),
    Unknown(UnknownPayload),
}
```

Read a text delta as `t.text`. Correlate `ToolCallDelta` fragments with the eventual
complete `ToolCall` through `internal_call_id` — it is stable across the call's fragments.
Buffer tool-call argument deltas until the call is complete; a partial JSON fragment is not
executable and is rarely worth showing.

`Unknown(UnknownPayload)` exists because providers add event kinds faster than crates
adopt them. Match it explicitly if you log; do not treat it as an error.

Two *kinds* of tool call never surface as a complete `ToolCall` item (their arguments
still stream as deltas): one rejected and handled by invalid-tool-call recovery, and a
structured-output `Tool`-mode output-tool call, which finalizes the run directly — its
result appears in `FinalResponse`.

## Low-Level Streaming

Below the agent, streaming lives on the **model**, not the agent: `Agent` has no
`stream_completion`. Build a request from a `CompletionModel` and stream it:

```rust
use rig::prelude::*;

let model = client.completion_model(MODEL);

let response = model
    .completion_request("Explain ownership in Rust.")
    .preamble("You are a patient teacher.".to_string())
    .temperature(0.9)
    .stream()
    .await?;
```

This returns a `StreamingCompletionResponse`, which wraps the inner chunk stream and, once
fully consumed, exposes the aggregated message and the raw provider response. Reach for it
when you want one request with no agent loop around it.

## Trait Mirrors

| Non-streaming | Streaming |
|---|---|
| `Prompt` | `StreamingPrompt` |
| `Chat` | `StreamingChat` |

Those are the only two streaming *traits* in 0.42. `rig::streaming` also re-exports the
streaming types from `rig-core` — `StreamedAssistantContent`, `StreamedUserContent`,
`StreamingCompletionResponse`, `ToolCallDeltaContent`, `PauseControl` and more — so the
module is where the item types live as well. Website references to a `Completion` /
`StreamingCompletion` trait pair do not apply.

## Practical Notes

- **Errors are per chunk.** Starting a stream always succeeds; each item is an independent
  `Result`. Match on `item?` rather than assuming the stream succeeds or fails atomically.
- **Read usage at the end.** The final usage event covers the whole completion, not the
  chunk.
- **Streaming hooks fire for deltas.** `on_text_delta`, `on_reasoning_delta`,
  `on_tool_call_delta`, and `on_stream_response_finish` are hot paths — override `observes`
  so Rig can skip dispatch when nothing cares. See
  Hooks and Runner.
- **The blocking and streaming paths behave identically** apart from the delta events:
  same run construction, tool execution, memory behavior, span shape, and hook handling.
  You can build one `AgentRunner` and choose `run()` or `stream()` at the end.
- **Apply backpressure** with ordinary stream discipline when the consumer cannot keep up:
  await your sink before pulling the next item, or push into a bounded
  `tokio::sync::mpsc` channel and let the send block. `rig::streaming::PauseControl`
  (`new` / `pause` / `resume` / `is_paused`) exists for user-controlled pause-and-resume,
  but 0.42's public API exposes no way to attach one to an agent stream — a `PauseControl`
  you construct yourself pauses nothing. Check the docs.rs page for your version before
  relying on it.
- **Cancel by dropping the stream.** There is no cancel method; dropping the
  `Stream` ends the run. Wrap the loop in `tokio::time::timeout` or `tokio::select!` with a
  shutdown signal if a stream must be bounded.
- **Do not render raw model text as HTML.** Streamed content is untrusted output; escape it
  the way you would any user-supplied string.

Back to the reference index in SKILL.md.
