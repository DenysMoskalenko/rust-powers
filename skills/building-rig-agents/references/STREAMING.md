# Streaming

Read this file when the user wants tokens as they arrive: a responsive UI, long-form
output, or live observation of tool activity.

Verified against `rig` 0.43.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [Stream an Agent](#stream-an-agent)
- [Streaming with History](#streaming-with-history)
- [What the Stream Yields](#what-the-stream-yields)
- [Low-Level Streaming](#low-level-streaming)
- [Where the Types Live](#where-the-types-live)
- [Practical Notes](#practical-notes)

## Stream an Agent

```rust,verify
use futures::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::prelude::*;
use rig::providers::openai::{self, OpenAI};
use rig::streaming::{Item, StreamEvent};
const MODEL: &str = openai::GPT_5_5;


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = AgentBuilder::new(OpenAI::from_env()?.completion(MODEL))
        .preamble("You are a storyteller.")
        .build();

    let mut stream = agent.prompt("Tell me a short story about a robot.").max_turns(1).stream();

    while let Some(item) = stream.next().await {
        match item? {
            MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text, ..
            })) => {
                print!("{text}");
            }
            MultiTurnStreamItem::FinalResponse(response) => {
                if let Some(total) = response.usage.total_tokens {
                    println!("\n[{total} tokens]");
                }
            }
            _ => {}
        }
    }

    Ok(())
}
```

`stream()` is the runner's streaming terminal: `.max_turns(..)`, `.conversation(..)`,
`.add_hook(..)` all chain on `agent.prompt(..)` first. It is synchronous and lazy — nothing
runs until the stream is polled.

For the common terminal case Rig ships a helper:

```rust
use rig::agent::stream_to_stdout;

let mut stream = agent.prompt("Hello!").max_turns(1).stream();
stream_to_stdout(&mut stream).await?;
```

It prints assistant text as it arrives and each reasoning part once it ends, and ignores tool calls,
which are rarely meaningful to display raw. A run error goes to stderr, not to the caller:
a failed run still returns `Ok`, with an empty response, and the `?` catches only I/O errors.

## Streaming with History

```rust
let mut stream = agent.prompt("Continue the story").history(&chat_history).max_turns(1).stream();
```

Note the asymmetry with blocking `chat`, which appends the committed turn to the `Vec` you
pass. A stream can be dropped half-way, so do not assume the turn was recorded because you
started one: record it yourself from `FinalResponse`'s `messages` (the run's turn, input
history excluded), or attach conversation memory and let Rig persist on success.

## What the Stream Yields

The whole agent loop flows through one stream, so you observe tool activity live, not just
text.

```rust
pub enum MultiTurnStreamItem {
    StreamAssistantItem(Item<StreamEvent>),
    ToolCall { tool_call: ToolCall },
    ToolExecutionCommitted { tool_call: ToolCall },
    StreamUserItem(StreamedUserContent),
    CompletionCall(CompletionCall),
    ModelTurnRetried { turn: usize },
    FinalResponse(PromptResponse),
}
```

- **`StreamAssistantItem`** — what the model emitted: each part's start, its text,
  reasoning or tool-call arguments, and its end.
- **`ToolCall`** — the complete tool call, when the model turn commits, for each call Rig
  routes to execution.
- **`ToolExecutionCommitted`** — confirmation that Rig executed and committed a tool call.
  This is *not* a real-time start notification: it surfaces together with its result only
  after the whole batch settles. For live host-side start/result observation, use tool
  hooks.
- **`CompletionCall`** — per-model-call detail: usage, finish reason, provider request id.
- **`ModelTurnRetried`** — a hook rejected the turn for a retry; the text and reasoning
  already streamed for it were provisional, so discard or reset them.
- **`FinalResponse`** — the completed run, carrying `output`, aggregated `usage`,
  `completion_calls`, and `messages`.

### `Item<StreamEvent>`

```rust
pub enum Item<E> {
    Event(E),
    Unknown(UnknownPayload),
}

pub enum StreamEvent {
    Start { part: Part, kind: PartKind },               // Text, Reasoning, ToolCall, Image
    Text { part: Part, text: String },
    Reasoning { part: Part, text: String },
    Arguments { part: Part, json: String },             // a tool call's arguments, once
    End { part: Part, content: AssistantContent },      // the finalized part
}
```

Read a text delta as `StreamEvent::Text { text, .. }`. Every event of one part carries the
same `part`, its position in the response. A tool call streams whole: its arguments arrive
once, as `Arguments`, and the complete call as `End` with `AssistantContent::ToolCall`.

`Item::Unknown(UnknownPayload)` exists because providers add event kinds faster than crates
adopt them. Match it explicitly if you log; do not treat it as an error.

Two *kinds* of tool call never surface as a `MultiTurnStreamItem::ToolCall`: one rejected
and handled by invalid-tool-call recovery, whose parts are held back and dropped too, and a
structured-output `Tool`-mode output-tool call, whose parts still stream and which
finalizes the run directly — its result appears in `FinalResponse`.

## Low-Level Streaming

Below the agent, streaming lives on the **model**, not the agent: `Agent` has no
`stream_completion`. Build a `CompletionRequest` and pass it to `model.stream(..)`:

```rust
use rig::completion::CompletionRequest;

let model = client.completion(MODEL);

let mut stream = model.stream(
    CompletionRequest::new("Explain ownership in Rust.").preamble("You are a patient teacher."),
)?;
// ...poll it like the agent stream, then:
let response = stream.finish().await?;
```

This returns a `CompletionStream` of `Item<StreamEvent>`s; `finish()` yields the same
`CompletionResponse` that `model.call(..)` would have returned, raw provider reply included.
Reach for it when you want one request with no agent loop around it.

## Where the Types Live

0.43 has no streaming *traits*: the old `Prompt` / `StreamingPrompt` and `Chat` /
`StreamingChat` pairs are gone (the `Chat` in `rig::integrations::cli_chatbot` only feeds the
REPL), and `stream()` is a method on the runner. `rig::streaming`
holds the item types — `Item`, `StreamEvent`, `Part`, `StreamedUserContent`,
`CompletionStream` and more — and `MultiTurnStreamItem` lives in `rig::agent`. Website
references to a `Completion` / `StreamingCompletion` trait pair do not apply.

## Practical Notes

- **Errors arrive as items.** Starting an agent stream always succeeds (`model.stream(..)`
  returns a `Result` for a request it cannot encode); a failure is an `Err` item, and the
  stream's last. Match on `item?` rather than assuming the stream succeeds or fails atomically.
- **Read usage from `CompletionCall` and `FinalResponse`.** There is no usage stream event:
  each `CompletionCall` carries one model call's usage, `FinalResponse`'s `usage` the run's.
- **Streaming hooks fire for deltas.** `on_text_delta` and `on_reasoning_delta` are hot
  paths (`on_tool_call_delta` fires once per call, with the whole arguments) — override `observes`
  so Rig can skip dispatch when nothing cares. See
  Hooks and Runner.
- **The blocking and streaming paths behave identically** apart from the delta events, the
  span name (`chat_streaming`, not `chat`) and malformed tool arguments, which reach
  `on_invalid_tool_call` only when streamed: same run construction, tool execution, memory
  behavior, span shape, and hook handling.
  You can build one `AgentRunner` and choose `run()` or `stream()` at the end.
- **Apply backpressure** with ordinary stream discipline when the consumer cannot keep up:
  await your sink before pulling the next item, or push into a bounded
  `tokio::sync::mpsc` channel and let the send block. 0.43 removed `PauseControl`: to pause,
  stop polling the stream.
- **Cancel by dropping the stream.** There is no cancel method; dropping the
  `Stream` ends the run. Wrap the loop in `tokio::time::timeout` or `tokio::select!` with a
  shutdown signal if a stream must be bounded.
- **Do not render raw model text as HTML.** Streamed content is untrusted output; escape it
  the way you would any user-supplied string.

Back to the reference index in SKILL.md.
