# Agents Core

Read this file when the user wants to create a provider client, configure an agent, pick
between `prompt` / `chat` / `prompt_typed`, set per-request options, or read token usage.

Verified against `rig` 0.42.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [Create a Provider Client](#create-a-provider-client)
- [Build an Agent](#build-an-agent)
- [The Agent Loop](#the-agent-loop)
- [Choosing an Interface](#choosing-an-interface)
- [Per-Request Options](#per-request-options)
- [Steering Tool Use](#steering-tool-use)
- [Token Usage and Run Details](#token-usage-and-run-details)
- [Messages](#messages)

## Create a Provider Client

```rust
use rig::prelude::*;
use rig::providers::openai;

let client = openai::Client::from_env()?;       // reads OPENAI_API_KEY
let client = openai::Client::new("sk-...")?;    // explicit key — also fallible
```

`from_env()` comes from `ProviderClient`; it returns `Err` at runtime when the variable is
missing, so a missing key compiles fine and fails on first use.

Rig ships clients for Anthropic, OpenAI, Azure OpenAI, Cohere, DeepSeek, Gemini, Groq,
HuggingFace, Hyperbolic, Moonshot, Ollama, OpenRouter, Perplexity, TogetherAI, xAI and
more. Each reads its own environment variable. For a local model use `providers::ollama`,
or point an OpenAI-compatible client at any endpoint implementing that API.

From a client you create models and higher-level constructs:

```rust
let model = client.completion_model(MODEL);                  // CompletionClient
let embed = client.embedding_model("text-embedding-3-small");    // EmbeddingsClient
let agent = client.agent(MODEL).build();                     // AgentClientExt
let extractor = client.extractor::<Person>(MODEL).build();   // AgentClientExt
```

Models are cheap handles over a shared HTTP client — build once, reuse across requests.

### Local and OpenAI-compatible endpoints

Anything speaking the OpenAI API — Ollama, vLLM, LiteLLM, a gateway, a self-hosted model —
works through the ordinary client with a different base URL. Use the typestate builder:

```rust
use rig::prelude::*;
use rig::providers::openai;

let client = openai::Client::builder()
    .api_key("not-used-but-required")
    .base_url("http://localhost:11434/v1")
    .build()?;

let agent = client.agent("llama3.1").preamble("You are helpful.").build();
```

`api_key(..)` must be called before `build()` is reachable — that is the typestate, not a
bug — so pass a placeholder for endpoints that do not authenticate. `ClientBuilder` also
takes `.http_client(..)` when you need your own transport (a proxy, custom timeouts,
instrumentation). Rig also ships a dedicated `providers::ollama` client.

### Swapping models at runtime

Because `AgentBuilder::new` erases the model into a `ModelHandle`, one `Agent` type can run
on any provider, and the model can change after construction. Three scopes, cheapest first:

| Scope | Call | Use for |
|---|---|---|
| One run | `.using_model(handle)` / `.using_model_value(model)` on a `PromptRequest` or `AgentRunner` | Per-request tiering: cheap model for simple queries |
| This agent, from now on | `agent.set_model(model)` / `set_model_handle(handle)`, or `with_model_handle(..)` to take it by value | A user-selected provider held for a session |
| Every model call in a run | an `on_model_select` hook | Fallback chains, A/B splits, cost ceilings mid-run |

```rust
let response = agent
    .runner("Summarize this ticket.")
    .using_model_value(cheap_model)
    .run()
    .await?;
```

Two properties worth knowing. Replacing a handle has value semantics — cloned agents keep
their own, and in-flight attempts never rebind — so a swap cannot corrupt a request already
running. And `ModelHandle` is deliberately **not** serializable, because it captures live
clients and credentials: persist your own model identifier and resolve it to a handle at
startup.

`using_model(..)` sets the run's default candidate but does not suppress model-selection
hooks; see Hooks and Runner.

## Build an Agent

```rust
let agent = client
    .agent(MODEL)
    .preamble("You are a helpful assistant.")
    .temperature(0.7)
    .max_tokens(1024)
    .build();
```

Or from a model directly, which is what you want in tests:

```rust
use rig::agent::AgentBuilder;

let agent = AgentBuilder::new(model).preamble("...").build();
```

`AgentBuilder::new` erases the model type into a `ModelHandle`, so the built value is
plain `Agent` — no type parameter, and agents from different providers share one type.

### Builder options

| Method | Effect |
|---|---|
| `preamble(&str)` / `append_preamble(&str)` / `without_preamble()` | System prompt |
| `context(&str)` | Add one static context document, sent on every request |
| `dynamic_context(samples, index)` | Retrieve `samples` documents from a vector index per model call |
| `name(&str)` / `description(&str)` | Become the tool name and description when this agent is handed to another via `into_tool()`; also label telemetry spans |
| `temperature(f64)`, `max_tokens(u64)` | Model parameters |
| `tool(T)` | Add a static tool (see Tools) |
| `tool_choice(ToolChoice)` | Force, forbid, or restrict tool use |
| `default_max_turns(usize)` | Default total model-call budget for every request |
| `additional_params(serde_json::Value)` | Provider-specific fields Rig does not model |
| `output_schema::<T>()` / `output_mode(OutputMode)` | Constrain responses to a schema |
| `memory(B)` / `conversation(id)` | Conversation memory backend and default id |
| `add_hook(H)` | Default hook applied to every request |
| `record_content_telemetry(bool)` | Opt into recording prompts/results on spans — off by default |

The tool methods use a typestate: after the first `.tool(..)` the builder becomes
`AgentBuilder<WithBuilderTools>` and `.tool_server_handle(..)` is no longer offered, and
vice versa. The two paths dispatch differently — builder tools go into an agent-owned
`ToolServer`, a handle points at someone else's — so mixing them would leave the agent with
two registries and no rule for which wins. If you need a shared server *and* a local tool,
register the local tool on the `ToolServer` before taking the handle.

## The Agent Loop

`.prompt(...)` does not make one request; it drives a loop:

1. Build a completion request from the preamble, static context, retrieved dynamic
   context, conversation history, and every advertised tool definition.
2. Send it. One request/response round trip is a **model call**.
3. If the model answered with text, return it. If it requested tool calls, run each one,
   append the results to the history, and go back to step 2.

The loop stops when the model produces text or the model-call budget runs out.

### Turn budget

`max_turns(n)` is the **total** model-call budget: the initial call plus every retry and
continuation. Zero permits no model call at all; one permits only the initial call. With
no `default_max_turns` set, the implicit budget is one — which is why an agent's first
tool-using prompt fails with `PromptError::MaxTurnsError` unless you raise it.

```rust
let answer = agent
    .prompt("Compute 2 + 5, then multiply by 3")
    .max_turns(5)
    .await?;
```

`chat(prompt, &mut history)` is the exception: it returns a plain future with no options, so
`agent.chat(..).max_turns(n)` does not compile and its budget is the agent's
`default_max_turns(n)`, set on the builder.

Pick a number matching the longest tool chain you actually expect, so a confused model
fails fast instead of burning tokens. `MaxTurnsError` carries the accumulated
`chat_history` and the undelivered `prompt`, so you can inspect what happened.

## Choosing an Interface

| You want… | Use |
|---|---|
| A text answer to a one-off prompt | `Prompt` — `.prompt(..)` |
| A conversation that carries history | `Chat` — `.chat(prompt, &mut history)` |
| A typed struct back | `TypedPrompt` — `.prompt_typed::<T>(..)` |
| Tokens as they arrive | `StreamingPrompt` — `.stream_prompt(..)` |
| Full control of one request | `CompletionModel` directly |

```rust
// One-shot
let text: String = agent.prompt("Hello").await?;

// Conversation — `chat` appends the committed turn to `history` for you
let mut history: Vec<Message> = Vec::new();
let text = agent.chat("Hello", &mut history).await?;

// Typed
let analysis: SentimentAnalysis = agent.prompt_typed("Analyze: 'I love this!'").await?;
```

Drop to a bare `CompletionModel` when you want to decide per tool result whether to return
it or feed it back — that is, when you are writing your own loop:

```rust
let model = client.completion_model(MODEL);
let response = model
    .completion_request("What is Rust?")
    .preamble("You are a helpful assistant.".to_string())
    .temperature(0.7)
    .max_tokens(1000)
    .send()
    .await?;
```

## Per-Request Options

`.prompt(..)` returns a `PromptRequest` builder; chain options before awaiting it.

| Method | Effect |
|---|---|
| `max_turns(n)` | Total model-call budget for this request |
| `history(iter)` | Explicit chat history — **bypasses conversation memory entirely** |
| `conversation(id)` | Conversation id for loading and saving memory |
| `extended_details()` | Return `PromptResponse` instead of `String` |
| `preamble(..)` / `without_preamble()` | Override the agent preamble for this request |
| `document(..)` / `documents(..)` | Append static context for this request |
| `temperature(..)`, `max_tokens(..)` | Override model parameters |
| `tool_choice(..)` / `without_tool_choice()` | Override the tool policy |
| `tool_context(ToolContext)` | Pass runtime-only values to tools (see Tools) |
| `add_hook(H)` | Append a hook for this request |
| `merge_additional_params(..)` / `replace_additional_params(..)` | Provider-specific fields |
| `record_content_telemetry(bool)` | Content telemetry for this request only |

The `without_*` family exists because per-request overrides are additive by default:
`without_temperature()` removes the agent's configured temperature rather than setting a
new one.

## Steering Tool Use

```rust
use rig::message::ToolChoice;

let agent = client
    .agent(MODEL)
    .preamble("You are a calculator. Always compute with tools.")
    .tool(Adder)
    .tool_choice(ToolChoice::Required)
    .build();
```

- `Auto` — the model may call tools or answer directly. This is the effective behavior
  when you set no tool choice at all, in which case Rig sends no tool-choice field and the
  provider's own default applies.
- `None` — tools are advertised but must not be called.
- `Required` — the model must call at least one tool.
- `Specific { function_names }` — the model must call one of the named tools.

`Required` and `Specific` are how you force a deterministic first step ("always search
before answering"). If you also narrow the advertised tool list, make sure the tool choice
still names a tool that is actually on offer.

## Token Usage and Run Details

`.prompt(..)` returns just the answer. Add `.extended_details()` to get a
`PromptResponse`:

```rust
let response = agent
    .prompt("What is 2 + 2?")
    .max_turns(3)
    .extended_details()
    .await?;

println!("answer: {}", response.output);
println!(
    "tokens: {} in / {} out over {} model calls",
    response.usage.input_tokens,
    response.usage.output_tokens,
    response.completion_calls.len(),
);
```

`usage` aggregates across every model call in the run. `completion_calls` breaks it down
per call, each entry carrying `call_index`, `usage`, `finish_reason`,
`provider_request_id`, and the raw provider payload — the last entry tells you how large
the final request's context was, and `finish_reason` tells you *which* call hit a token
limit. `messages` is `Option<Vec<Message>>` — the full history the run produced, or `None` when
history bookkeeping was disabled for the request.

Zero-valued usage is Rig's documented sentinel for "the provider did not report metrics";
it does not distinguish that from a genuine zero. So do not sum `usage` for billing or
budget enforcement without checking whether the call reported anything at all —
`finish_reason` and `provider_request_id` on the same entry tell you a call happened.

## Messages

```rust
pub enum Message {
    System    { content: String },
    User      { content: Vec<UserContent> },
    Assistant { id: Option<String>, content: Vec<AssistantContent> },
}
```

`UserContent` covers text, tool results, images, audio, documents, and video.
`AssistantContent` covers text, tool calls, reasoning, and images. For the ordinary text case use
the constructors rather than building the enum by hand:

```rust
use rig::completion::Message;

history.push(Message::user("What is Rust?"));
history.push(Message::assistant("Rust is a systems programming language..."));
```

`Message` is `Serialize`/`Deserialize`, so persisting a conversation is ordinary serde
work.

### Multimodal input

A user message carries a `Vec<UserContent>`, so text and media go in the same turn:

```rust
use rig::completion::Message;
use rig::message::{ImageDetail, ImageMediaType, UserContent};

let message = Message::User {
    content: vec![
        UserContent::text("What is in this image?"),
        UserContent::image_url(
            "https://example.com/chart.png",
            Some(ImageMediaType::PNG),
            Some(ImageDetail::Auto),
        ),
    ],
};

let answer = agent.prompt(message).await?;
```

Every media constructor takes its media type as an `Option`, so pass `None` when you want
the provider to sniff it. Image constructors take a second `Option<ImageDetail>`
(`Low` / `High` / `Auto`) that the others do not:

| Constructor | Signature |
|---|---|
| `image_url`, `image_base64`, `image_raw` | `(data, Option<ImageMediaType>, Option<ImageDetail>)` |
| `audio_url`, `audio`, `audio_raw` | `(data, Option<AudioMediaType>)` |
| `video_url`, `video`, `video_raw` | `(data, Option<VideoMediaType>)` |
| `document_url`, `document`, `document_raw` | `(data, Option<DocumentMediaType>)` |

The `_raw` variants take unencoded `Vec<u8>`; the bare and `_base64` names take base64.

Support is per-provider, and content a provider cannot accept is dropped during
translation rather than rejected — a silent downgrade, so test the combination you ship.
Image and audio *generation*, as opposed to input, is a separate surface behind the `image`
and `audio` features.

Back to the reference index in SKILL.md.
