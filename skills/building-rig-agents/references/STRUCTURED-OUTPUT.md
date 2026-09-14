# Structured Output

Read this file when the user wants typed data out of a model instead of a string:
extraction, classification, scoring, or any agent step whose result feeds code rather than
a human.

Verified against `rig` 0.42.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [Three Surfaces](#three-surfaces)
- [Extractor](#extractor)
- [TypedPrompt](#typedprompt)
- [Schema-Constrained Agents](#schema-constrained-agents)
- [Designing Schemas the Model Can Fill](#designing-schemas-the-model-can-fill)
- [Batch Extraction](#batch-extraction)

## Three Surfaces

| You want | Use |
|---|---|
| Structured extraction *is* the job — parse text into a type | `Extractor<T>` |
| One step of a broader agent workflow returns a type | `TypedPrompt` — `agent.prompt_typed::<T>(..)` |
| An agent whose every answer matches a schema | `AgentBuilder::output_schema::<T>()` |

All three require the target type to derive `serde::Deserialize` and
`schemars::JsonSchema`; `Extractor` additionally requires `Serialize`.

Import it as `use rig::schemars::{self, JsonSchema};` rather than adding your own
`schemars` dependency: Rig needs v1, a second copy in the graph causes confusing trait
mismatches, and the `self` is load-bearing — the derive macro's generated code needs a
`schemars` name in scope. Field descriptions come from `///` doc comments;
`#[schemars(description = "…")]` also works and wins when both are present.

## Extractor

```rust,verify
use rig::prelude::*;
use rig::providers::openai;
use rig::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
const MODEL: &str = openai::GPT_5_5;


#[derive(Deserialize, Serialize, JsonSchema)]
struct Person {
    /// Full name exactly as written in the text.
    name: Option<String>,
    /// Age in years.
    age: Option<u8>,
    /// Occupation or job title.
    profession: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let extractor = openai::Client::from_env()?
        .extractor::<Person>(MODEL)
        .preamble("Extract person details with high precision.")
        .context("Ages are given in years; ignore honorifics like 'Dr.'")
        .build();

    let person = extractor.extract("John Doe is a 30 year old doctor.").await?;
    println!("{:?}", person.name);

    Ok(())
}
```

Under the hood an extractor is an agent plus a private "submit" tool whose arguments are
your target type. Rig generates the JSON schema from the struct at compile time, the model
calls the submit tool, and Rig deserializes the arguments back into your type.

### Builder options

| Method | Effect |
|---|---|
| `preamble(&str)` | Steer the extraction |
| `context(&str)` | Add a static context document |
| `dynamic_context(samples, index)` | Retrieve context from a vector index per attempt |
| `max_tokens(u64)`, `additional_params(Value)` | Model parameters |
| `tool_choice(ToolChoice)` | Tool policy for the inner agent |
| `retries(u64)` | Maximum retry attempts |
| `add_hook(H)` | Lifecycle hook on every extraction attempt |

### Extraction methods

| Method | Returns |
|---|---|
| `extract(text)` | `Result<T, ExtractionError>` |
| `extract_with_usage(text)` | The value plus token usage |
| `extract_with_chat_history(text, history)` | Extraction in conversational context |
| `extract_with_chat_history_with_usage(..)` | Both |

`using_model(handle)` and `using_model_value(model)` are not extraction methods: each
returns an `ExtractorRun<'_, T>` on which you then call one of the four above, so a single
extractor can serve one run with a different model without being rebuilt.
`with_model_handle(..)` changes the extractor's own default instead.

### Error handling

```rust
use rig::extractor::ExtractionError;

match extractor.extract(text).await {
    Ok(person) => { /* use person */ }
    Err(ExtractionError::NoData) => {
        eprintln!("model never produced structured data");
    }
    Err(ExtractionError::DeserializationError(e)) => {
        eprintln!("submitted JSON did not match the type: {e}");
    }
    Err(err) => return Err(err.into()),
}
```

`ExtractionError` is `NoData`, `DeserializationError`, `CompletionError`, `PromptError`.

`NoData` means the model never called the submit tool, so nothing was generated. Rule out
the cheap causes first — a `ToolChoice` that forbids the tool, a preamble that discourages
tool use, an input with nothing to extract — and only then reach for a more capable model.

## TypedPrompt

When structured output is one step inside a larger agent workflow, skip the extractor and
ask the agent directly.

```rust
use rig::prelude::*;
use rig::schemars::{self, JsonSchema};
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct SentimentAnalysis {
    /// Sentiment score from -1.0 (negative) to 1.0 (positive).
    score: f64,
    /// One of: positive, negative, neutral.
    label: String,
}

let result: SentimentAnalysis = agent
    .prompt_typed("Analyze the sentiment of: 'I love this product!'")
    .await?;
```

`prompt_typed` returns `Result<T, StructuredOutputError>`:

```rust,verify
use rig::completion::PromptError;
pub enum StructuredOutputError {
    PromptError(Box<PromptError>),          // the run itself failed
    DeserializationError(serde_json::Error), // JSON did not match T
    EmptyResponse,                           // the model returned nothing to parse
}
```

`EmptyResponse` is the `TypedPrompt` analogue of the extractor's `NoData` — the run
succeeded but produced no structured payload. Handle it separately from a deserialization
failure: one means "ask a better model", the other means "fix the schema".

## Schema-Constrained Agents

To make *every* answer from an agent conform to a schema, set it on the builder:

```rust
use rig::agent::OutputMode;

let agent = client
    .agent(MODEL)
    .preamble("Classify each support ticket.")
    .output_schema::<TicketClassification>()
    .output_mode(OutputMode::Native)
    .build();
```

`output_schema_raw(schema)` takes any `schemars::Schema` when you build the schema at
runtime rather than from a type.

### Choosing an output mode

| Mode | Enforcement | When |
|---|---|---|
| `Auto` (default) | Provider-aware | Leave it unless you need the hard guarantee `Native` gives |
| `Native` | **Guaranteed** by the provider | You need a hard guarantee and the provider supports it |
| `Tool` | Best-effort — schema offered as a tool | Providers whose native constraint would suppress tool calls |
| `Prompted` | Best-effort — schema described in the prompt | Fallback for providers with neither |

`Native` is the only mode where the provider constrains the response. `Tool` and
`Prompted` *ask* the model to honor the schema: Rig re-prompts a bounded number of times,
but you should still validate the returned JSON before relying on it.

`Auto` resolves at request time. For an agent with both an `output_schema` and function
tools, it routes to `Tool` only on providers whose native constraint would suppress tool
calls, and keeps guaranteed `Native` output on providers that compose the two (OpenAI,
Anthropic). Setting the mode has no effect unless a schema is also set.

## Designing Schemas the Model Can Fill

- **Use `Option<T>` for anything that may be absent.** It lets the model omit a field
  cleanly instead of inventing a value.
- **Keep structs small and focused.** Every nested level and extra field is one more thing
  the model has to get right in a single pass. When a schema is big enough that partial
  results are common, two passes over flat schemas usually beat one over a deep one.
- **Document every field with `///`.** Those doc comments become the schema descriptions
  the model actually reads.
- **Use an enum for fixed choices** so the model cannot invent a category. A `String`
  field named `label` will eventually come back as `"Positive "` or `"mostly positive"`.
- **Prefer scalars over nested objects** wherever the domain allows it.

## Batch Extraction

Extractors are cheap to reuse — build once, extract in a loop:

```rust
for doc in &docs {
    results.push(extractor.extract(doc).await);
}
```

For throughput, bound concurrency yourself — `futures::stream::iter(..).buffer_unordered(n)`
with a small `n` keeps you inside provider rate limits. Rig does not throttle for you.

Document loaders pair naturally with extractors; see
Orchestration.

Back to the reference index in SKILL.md.
