# Structured Output

Read this file when the user wants typed data out of a model instead of a string:
extraction, classification, scoring, or any agent step whose result feeds code rather than
a human.

Verified against `rig` 0.43.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [Three Surfaces](#three-surfaces)
- [Extractor](#extractor)
- [Typed Prompts](#typed-prompts)
- [Schema-Constrained Agents](#schema-constrained-agents)
- [Designing Schemas the Model Can Fill](#designing-schemas-the-model-can-fill)
- [Batch Extraction](#batch-extraction)

## Three Surfaces

| You want | Use |
|---|---|
| Structured extraction *is* the job — parse text into a type | `Extractor<T>` |
| One step of a broader agent workflow returns a type | `agent.prompt_typed::<T>(..)` |
| An agent whose every answer matches a schema | `AgentBuilder::output_schema::<T>()` |

The extractor forces its submit tool with `ToolChoice::Required`. Claude Opus 5.5, Sonnet
5.5 and Fable 5.1 reject a forced tool choice, so on those models rig 0.43 drops the force
and asks for native output instead, which rig sends to Anthropic as `output_config.format`;
a forced choice you set on an agent yourself still reaches them, and is still a 400.
`prompt_typed` always runs in `OutputMode::Native`, so the provider guarantees the schema on
every model that supports it.

`Extractor` and `prompt_typed` require the target type to derive `serde::Deserialize` and
`schemars::JsonSchema`, and `Extractor` additionally `Serialize`; `output_schema::<T>()` needs
only `JsonSchema`.

Import it as `use rig::schemars::{self, JsonSchema};` rather than adding your own
`schemars` dependency: Rig needs v1, a second copy in the graph causes confusing trait
mismatches, and the `self` is load-bearing — the derive macro's generated code needs a
`schemars` name in scope. Field descriptions come from `///` doc comments;
`#[schemars(description = "…")]` also works and wins when both are present.

## Extractor

```rust,verify
use rig::extractor::ExtractorBuilder;
use rig::providers::openai::{self, OpenAI};
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
    let extractor = ExtractorBuilder::<Person>::new(OpenAI::from_env()?.completion(MODEL))
        .append_preamble("Extract person details with high precision.")
        .context("Ages are given in years; ignore honorifics like 'Dr.'")
        .build();

    let person = extractor.extract("John Doe is a 30 year old doctor.").await?.output;
    println!("{:?}", person.name);

    Ok(())
}
```

Under the hood an extractor is an agent plus a private "submit" tool whose arguments are
your target type. Rig builds the JSON schema from the struct's `JsonSchema` impl at runtime, the model
calls the submit tool, and Rig deserializes the arguments back into your type.

### Builder options

| Method | Effect |
|---|---|
| `append_preamble(&str)` | Steer the extraction; the extraction preamble itself is fixed |
| `context(&str)` | Add a static context document |
| `dynamic_context(samples, index)` | Retrieve context from a vector index per attempt |
| `max_tokens(u64)`, `additional_params(Value)` | Model parameters |
| `tool_choice(ToolChoice)` | Tool policy for the inner agent |
| `retries(u64)` | Maximum retry attempts |
| `add_hook(H)` | Lifecycle hook on every extraction attempt |

### Extraction methods

`extract(text)` returns a `TypedRun<T>`; awaiting it gives
`Result<TypedPromptResponse<T>, StructuredOutputError>`, whose `output` is the value and
whose `usage` covers every attempt and `completion_calls` the accepted one. Before awaiting, `.history(h)` adds
conversational context, and `.using_model(label)` or `.using_model_value(model)` serves one
run with a different model without rebuilding the extractor. `with_model(..)` changes the
extractor's own default instead.

### Error handling

```rust
use rig::completion::StructuredOutputError;

match extractor.extract(text).await {
    Ok(response) => { /* use response.output */ }
    Err(StructuredOutputError::EmptyResponse) => {
        eprintln!("model never produced structured data");
    }
    Err(StructuredOutputError::DeserializationError(e)) => {
        eprintln!("submitted JSON did not match the type: {e}");
    }
    Err(err) => return Err(err.into()),
}
```

The extractor fails with the same `StructuredOutputError` as `prompt_typed`, below.

`EmptyResponse` means no submit-tool call produced a value. On Opus 5.5, Sonnet 5.5 and Fable
5.1, where the extractor asks for native output, it also covers a JSON answer that does not fit
`T`, so check the schema there first. Otherwise rule out the cheap causes first — a `ToolChoice` that forbids the tool, a preamble that discourages
tool use, an input with nothing to extract — and only then reach for a more capable model.

## Typed Prompts

When structured output is one step inside a larger agent workflow, skip the extractor and
ask the agent directly.

```rust
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
    .max_turns(1)
    .await?
    .output;
```

`prompt_typed` returns `Result<TypedPromptResponse<T>, StructuredOutputError>`:

```rust
pub enum StructuredOutputError {
    PromptError(PromptError),               // the run itself failed
    DeserializationError(serde_json::Error), // JSON did not match T
    EmptyResponse,                           // the model returned nothing to parse
}
```

`EmptyResponse` means the run succeeded but produced no structured payload. Handle it
separately from a deserialization failure: one means "ask a better model", the other means
"fix the schema".

## Schema-Constrained Agents

To make *every* answer from an agent conform to a schema, set it on the builder:

```rust
use rig::agent::{AgentBuilder, OutputMode};

let agent = AgentBuilder::new(client.completion(MODEL))
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
`Prompted` *ask* the model to honor the schema. Only `Tool` re-prompts, once by default and
only while the turn budget allows; `Prompted` returns the text verbatim, prose and markdown
included. Validate the returned JSON before relying on it.

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
