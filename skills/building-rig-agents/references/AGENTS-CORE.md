# Agents Core

Read this file when the user wants to create a provider client, configure an agent, pick
between `prompt` / `chat` / `prompt_typed`, set per-request options, or read token usage.

Verified against `rig` 0.43.0.

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
use rig::providers::openai::{self, OpenAI};

let client = OpenAI::from_env()?;       // reads OPENAI_API_KEY
let client = OpenAI::new("sk-...");     // explicit key — infallible
```

`from_env()` is inherent on each client; it returns `Err` at runtime when the variable is
missing, so a missing key compiles fine and fails on first use.

Rig ships clients for Anthropic, OpenAI, Azure OpenAI, Cohere, DeepSeek, Gemini, Groq,
HuggingFace, Hyperbolic, Moonshot, Ollama, OpenRouter, Perplexity, TogetherAI, xAI and
more. Each reads its own environment variable. The OpenAI-compatible ones (Azure, DeepSeek,
Groq, OpenRouter, TogetherAI, xAI, …) have no client type: `deepseek::from_env()?` is a module
function returning an `OpenAI`. For a local model use `providers::ollama`,
or point an OpenAI-compatible client at any endpoint implementing that API.

From a client you create models, and from a model the higher-level constructs:

```rust
let model = client.completion(MODEL);
let embed = client.embedding("text-embedding-3-small", None);  // `Some(n)` sets the width
let agent = AgentBuilder::new(client.completion(MODEL)).build();
let extractor = ExtractorBuilder::<Person>::new(client.completion(MODEL)).build();
```

Models are cheap handles over a shared HTTP client — build once, reuse across requests.

### Local and OpenAI-compatible endpoints

Anything speaking the OpenAI API — Ollama, vLLM, LiteLLM, a gateway, a self-hosted model —
works through the ordinary client with a different base URL. Set it on the configuration:

```rust
use rig::prelude::*;
use rig::providers::openai::{OpenAIConfig, Route};

let client = OpenAIConfig::new("not-used-but-required")
    .with_base_url("http://localhost:11434/v1")
    .with_route(Route::Chat)
    .client();

let agent = AgentBuilder::new(client.completion("llama3.1")).preamble("You are helpful.").build();
```

`OpenAIConfig::new` takes a key, so pass a placeholder for endpoints that do not
authenticate. It also targets OpenAI itself, whose `completion(..)` route is the Responses
API; most compatible servers implement only Chat Completions and answer that route with a 404,
so `Route::Chat` sends `completion(..)` to `/chat/completions` (`client.chat(m)` does the same
for one model). Every client also takes `.with_http(..)` when you need your own transport (a
proxy, custom timeouts, instrumentation). Rig also ships a dedicated `providers::ollama`
client.

### Swapping models at runtime

Because `AgentBuilder::new` erases the model into a `DynModel`, one `Agent` type can run
on any provider, and the model can change after construction. Three scopes, cheapest first:

| Scope | Call | Use for |
|---|---|---|
| One run | `.using_model_value(model)`, or `.using_model(label)` for one registered with `AgentBuilder::model_route(label, model)` | Per-request tiering: cheap model for simple queries |
| This agent, from now on | `agent.set_model(model)` / `set_model_label(label)`, or `with_model(..)` to take it by value | A user-selected provider held for a session |
| Every model call in a run | an `on_model_select` hook returning `ModelSelectionAction::select(label)` | Fallback chains, A/B splits, cost ceilings mid-run |

```rust
let response = agent
    .prompt("Summarize this ticket.")
    .using_model_value(cheap_model)
    .max_turns(1)
    .await?;
```

Two properties worth knowing. Replacing a handle has value semantics — cloned agents keep
their own, and in-flight attempts never rebind — so a swap cannot corrupt a request already
running. And a `DynModel` is deliberately **not** serializable, because it captures live
clients and credentials: persist your own model identifier and resolve it to a model at
startup.

`using_model(..)` sets the run's default candidate but does not suppress model-selection
hooks; see Hooks and Runner.

## Build an Agent

```rust
use rig::agent::AgentBuilder;

let agent = AgentBuilder::new(client.completion(MODEL))
    .preamble("You are a helpful assistant.")
    .max_tokens(16_000)
    .build();
```

The same constructor takes a mock model in tests. `AgentBuilder::new` erases the model type
into a `DynModel`, so the built value is plain `Agent` — no type parameter, and agents from
different providers share one type.

### Builder options

| Method | Effect |
|---|---|
| `preamble(&str)` / `append_preamble(&str)` / `without_preamble()` | System prompt |
| `context(&str)` | Add one static context document, sent on every request |
| `dynamic_context(samples, index)` | Retrieve `samples` documents from a vector index per model call |
| `name(&str)` / `description(&str)` | Become the tool name (default `agent_tool`) and description when this agent is handed to another via `into_tool()`, whose description also carries the preamble; also label telemetry spans |
| `temperature(f64)`, `max_tokens(u64)` | Model parameters. Claude Opus 4.7 and later, Sonnet 5 and Fable reject `temperature` with a 400. On Claude, `max_tokens` covers thinking plus the reply; rig 0.43 defaults it to the published output limit of the Claude ids it knows (128K from Opus 4.6 and Sonnet 4.6 on) and, when `max_tokens` is unset, fails every prompt to an id it does not know |
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
| A text answer to a one-off prompt | `.prompt(..)` |
| A conversation that carries history | `.chat(prompt, &mut history)` |
| A typed struct back | `.prompt_typed::<T>(..)` |
| Tokens as they arrive | `.prompt(..).stream()` |
| Full control of one request | the model's `call(..)` directly |

`prompt`, `chat` and `prompt_typed` are inherent methods on `Agent`, and `stream()` is on the
runner `prompt(..)` returns; rig 0.43 has no `Prompt` or `TypedPrompt` trait to import, and its
one `Chat` trait, `rig::integrations::cli_chatbot::Chat`, only puts something other than an
agent behind the REPL.

```rust
// One-shot
let text: String = agent.prompt("Hello").max_turns(1).await?.output;

// Conversation — `chat` appends the committed turn to `history` for you
let mut history: Vec<Message> = Vec::new();
let text = agent.chat("Hello", &mut history).await?.output;

// Typed
let analysis: SentimentAnalysis = agent.prompt_typed("Analyze: 'I love this!'").max_turns(1).await?.output;
```

Drop to a bare model when you want to decide per tool result whether to return it or feed
it back — that is, when you are writing your own loop:

```rust
use rig::completion::CompletionRequest;

let model = client.completion(MODEL);
let request = CompletionRequest::new("What is Rust?")
    .preamble("You are a helpful assistant.")
    .max_tokens(16_000);
let response = model.call(request).await?;
```

## Per-Request Options

`.prompt(..)` returns an `AgentRunner`; chain options before awaiting it.

| Method | Effect |
|---|---|
| `max_turns(n)` | Total model-call budget for this request |
| `history(iter)` | Explicit chat history — **bypasses conversation memory entirely** |
| `conversation(id)` | Conversation id for loading and saving memory |
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
let agent = AgentBuilder::new(client.completion(MODEL))
    .preamble("You are a calculator. Compute every sum with the `add` tool.")
    .tool(Adder)
    .build();
```

- `Auto` — the model may call tools or answer directly. This is the effective behavior
  when you set no tool choice at all, in which case Rig sends no tool-choice field and the
  provider's own default applies.
- `None` — tools are advertised but must not be called.
- `Required` — the model must call at least one tool.
- `Specific { function_names }` — the model must call one of the named tools.

`Required` and `Specific` force a call. rig sends them to Anthropic as `any` and `tool`, and
Claude Opus 5.5, Sonnet 5.5 and Fable 5.1 reject both with a 400. For a step that must come first on
every provider ("always search before answering"), name the tool in the preamble under
`Auto` and check in an `on_dispatch` hook that the call happened. If you also narrow the
advertised tool list, make sure the tool choice still names a tool that is actually on offer.

## Token Usage and Run Details

Every run returns a `PromptResponse`, the answer with its details:

```rust
let response = agent
    .prompt("What is 2 + 2?")
    .max_turns(3)
    .await?;

println!("answer: {}", response.output);
println!(
    "tokens: {:?} in / {:?} out over {} model calls",
    response.usage.input_tokens,
    response.usage.output_tokens,
    response.completion_calls.len(),
);
```

`usage` aggregates across every model call in the run. `completion_calls` breaks it down
per call, each entry carrying `call_index`, `usage`, `finish_reason`,
`provider_request_id`, and the raw provider payload — the last entry tells you how large
the final request's context was, and `finish_reason` tells you *which* call hit a token
limit. `messages` is `Option<Vec<Message>>` — the messages the run committed, prompt
included and input history excluded, or `None` for a response built without a run behind it.

Every counter is an `Option<u64>`: `None` means the provider did not report it, `Some(0)` is a
genuine zero, and `usage.is_reported()` says whether anything was reported at all. On
Anthropic `input_tokens` includes cache reads and writes, so adding `cached_input_tokens` to
it double counts.

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

let answer = agent.prompt(message).max_turns(1).await?.output;
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

The `_raw` variants take unencoded `Vec<u8>`; `image_base64`, `audio` and `video` take base64,
and `document` takes a plain string, sent as is.

Support is per-provider, and content a provider cannot accept fails the request with a
conversion error before anything is sent, so test the combination you ship.
Image and audio *generation*, as opposed to input, is a separate surface behind the `image`
and `audio` features.

Back to the reference index in SKILL.md.
