# Orchestration

Read this file when the user wants more than one model call wired together: workflows,
model routing, multi-agent systems, runtime provider selection, document loading, or an
interactive REPL.

Verified against `rig` 0.45.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [Agent or Workflow?](#agent-or-workflow)
- [Workflows Are Plain Rust](#workflows-are-plain-rust)
- [Model Routing](#model-routing)
- [Multi-Agent Systems](#multi-agent-systems)
- [Loaders](#loaders)
- [Interactive REPL](#interactive-repl)

## Agent or Workflow?

The central design decision is *who controls the flow*.

- **An agent with tools lets the model decide** — which tool, in what order, when to stop.
  Its path differs on every run. Use it for open-ended problems whose steps you cannot
  enumerate.
- **A workflow puts your code in charge.** The steps and routing are ordinary Rust. Each
  step may call a model, but no model decides what happens next. Use it when the process is
  known: it is cheaper (no turns spent deliberating), predictable, and testable step by
  step.

Prefer the least agentic design that solves the problem. If a step needs no model, write a
function. If the steps are known, write a workflow. Reserve the agent loop for the parts
that genuinely need the model's judgment. The two compose freely — a workflow step can be a
tool-using agent.

## Workflows Are Plain Rust

Rig has no workflow DSL. An earlier experimental `pipeline` module (the `Op` trait,
`pipeline::new`, the `parallel!` macro) was removed; the patterns below replace it.

### Sequential

```rust
let draft = writer.prompt("A tool that compile-checks docs code samples").max_turns(1).await?.output();
let tagline = editor.prompt(draft.trim()).max_turns(1).await?.output();
```

### Parallel

```rust
let review = "The new update is fast, but the settings menu is confusing.";

let (sentiment, topic) = futures::join!(
    async { sentiment_agent.prompt(review).max_turns(1).await.map(|r| r.output()) },
    async { topic_agent.prompt(review).max_turns(1).await.map(|r| r.output()) },
);

// Handle each independently — a `?` here would throw away the branch that succeeded.
match (sentiment, topic) {
    (Ok(s), Ok(t)) => println!("sentiment={s}, topic={t}"),
    (Ok(s), Err(e)) => println!("sentiment={s}, topic unavailable: {e}"),
    (Err(e), Ok(t)) => println!("sentiment unavailable: {e}, topic={t}"),
    (Err(a), Err(b)) => eprintln!("both failed: {a} / {b}"),
}
```

`join!` awaits both and hands back each `Result`; unlike `try_join!`, one failure does not
discard the other's work — provided you do not immediately `?` it away.

### Conditional routing

```rust
let category = router.prompt(query).max_turns(1).await?.output();

let answer = match category.trim().to_lowercase().as_str() {
    "code" => coder.prompt(query).max_turns(1).await?.output(),
    "math" => mathematician.prompt(query).max_turns(1).await?.output(),
    _ => generalist.prompt(query).max_turns(1).await?.output(),
};
```

String matching on model output is brittle — `"Code"` or `"it's a code question"` falls
through to the generalist. For production, classify with a typed extractor into an enum;
see [Model Routing](#model-routing) below.

### Evaluator-optimizer

```rust
let mut draft = writer.prompt(brief).max_turns(1).await?.output();

for _ in 0..3 {                                  // always bound the loop
    let review = critic.prompt(draft.as_str()).max_turns(1).await?.output();
    if review.trim().starts_with("APPROVED") {
        break;
    }
    let revision = format!("Revise this text:\n{draft}\n\nAddress this feedback:\n{review}");
    draft = writer.prompt(revision.as_str()).max_turns(1).await?.output();
}
```

This is the workflow-shaped cousin of the agent loop: the iteration structure is fixed in
your code, only the content comes from the model.

### Failures mid-workflow

Every step returns a `Result`, so error handling is ordinary Rust: `?` aborts the chain,
`match` substitutes a fallback, and a retry re-runs one step. Apply retries **per step, not
around the whole workflow**, so a retry never re-runs a step that already succeeded and had
side effects.

## Model Routing

Routing puts several specialized agents behind one interface. It doubles as a guardrail
(only answer certain topics) and a cost lever (cheap model for simple queries).

### Typed registry

Because `Agent` is not generic, a plain map holds agents from **any**
provider:

```rust,verify
use rig::agent::Agent;
use std::collections::HashMap;

struct Router {
    routes: HashMap<String, Agent>,
}

impl Router {
    fn new() -> Self {
        Self { routes: HashMap::new() }
    }

    fn add_route(mut self, name: &str, agent: Agent) -> Self {
        self.routes.insert(name.to_string(), agent);
        self
    }

    fn fetch(&self, route: &str) -> Option<&Agent> {
        self.routes.get(route)
    }
}
```

This is a genuine simplification over older Rig: the enum-dispatch and provider-registry
gymnastics the website describes for mixing providers are no longer needed. A
`HashMap<String, Agent>` can hold an OpenAI agent and an Anthropic agent side by side.

### Classifying reliably

Prefer a typed classifier over raw string matching:

```rust
use rig::extractor::ExtractorBuilder;
use rig::providers::openai;
use rig::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, JsonSchema)]
enum Route {
    Rust,
    Math,
    General,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct Classification {
    /// Which specialist should handle this query.
    route: Route,
}

let classifier = ExtractorBuilder::<Classification>::new(client.completion(openai::GPT_5_MINI)).build();
let classification = classifier.extract(query).await?.output;

let answer = match classification.route {
    Route::Rust => coding_agent.prompt(query).max_turns(1).await?.output(),
    Route::Math => math_agent.prompt(query).max_turns(1).await?.output(),
    Route::General => generalist.prompt(query).max_turns(1).await?.output(),
};
```

An enum makes an unexpected label impossible rather than merely unlikely, and the `match` is
exhaustive, so adding a route is a compile error until you handle it. For a handful of
well-separated routes a small cheap model does the classification fine; blurry or numerous
routes are where semantic routing earns its setup.

### Semantic routing

For reliability without an LLM call, embed each route's name, description, and example
queries, then match the incoming query by vector similarity:

```rust
let req = VectorSearchRequest::builder().query(query).samples(1).build();
let results = route_index.top_n::<RouteDefinition>(req).await?;

let route = results
    .first()
    .map(|result| result.document.name.as_str())
    .unwrap_or("general");
```

Always provide a fallback route for queries that match nothing, and set a similarity
threshold below which you route to the generalist rather than to a bad specialist. Pick the
threshold from a labelled sample: run your known-general queries through the index and take
the score they fall below.

## Multi-Agent Systems

### Do you need one?

If your workflow lives in one domain with under ~10–15 tools, better prompting and context
engineering will beat the complexity of coordinating agents. Try structured outputs, better
retrieval, and tighter role definitions first.

Multi-agent pays off when you have 20+ tools and the model picks wrong ones, genuine
cross-domain coordination, context-window exhaustion retrieval cannot solve, or clear
role-delegation boundaries. If you cannot articulate why you need several agents, use one.

### Manager-worker

An agent can be converted into a tool and handed to another agent. Note the conversion:
`Agent` does **not** implement `Tool` (that trait needs a `const NAME`, and an agent's
name is runtime state). Use `Agent::into_tool()?`, which yields a `DynamicTool`, and attach
it with `.dynamic_tool(..)` rather than `.tool(..)`.

```rust,verify,test
use rig::DynModel;
use rig::agent::{Agent, AgentBuilder};
use rig::message::EmptyToolName;
use rig::operation::Completion;
use rig::tool::ToolExecutionError;

#[rig::rig_tool(description = "Look up an order's status by its id")]
async fn order_status(id: String) -> Result<String, ToolExecutionError> {
    Ok(format!("order {id} has shipped"))
}

/// A manager that hands order questions to a worker agent.
fn support_team(
    manager: impl Into<DynModel<Completion>>,
    worker: impl Into<DynModel<Completion>>,
) -> Result<Agent, EmptyToolName> {
    let orders = AgentBuilder::new(worker)
        .name("orders")
        .description("Answers a question about one order, given its id.")
        .preamble("Look the order up with `order_status` and report what it says.")
        .tool(OrderStatus)
        // The manager's `.max_turns(..)` does not reach the worker: `into_tool` runs
        // it on its own default, and one call cannot fit a tool call plus an answer.
        .default_max_turns(3)
        .build();

    Ok(AgentBuilder::new(manager)
        .name("support")
        .preamble("You answer support questions. Ask `orders` about any order.")
        .dynamic_tool(orders.into_tool()?)
        .build())
}

#[cfg(test)]
mod tests {
    use rig::test_utils::{MockCompletionModel, MockTurn};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn the_worker_runs_on_its_own_turn_budget() {
        let worker = MockCompletionModel::from_turns([
            MockTurn::tool_call("call_1", "order_status", json!({ "id": "42" })),
            MockTurn::text("Order 42 has shipped."),
        ]);
        let manager = MockCompletionModel::from_turns([
            MockTurn::tool_call("call_2", "orders", json!({ "prompt": "Where is order 42?" })),
            MockTurn::text("It has shipped."),
        ]);
        let (worker_probe, manager_probe) = (worker.clone(), manager.clone());
        let team = support_team(manager, worker).unwrap();

        team.prompt("Where is order 42?").max_turns(3).await.unwrap();

        assert_eq!(worker_probe.request_count(), 2, "a tool call, then the answer");
        // The worker's answer is the tool result the manager's second call reads.
        let delegated = format!("{:?}", manager_probe.requests()[1].chat_history.last());
        assert!(delegated.contains("Order 42 has shipped."), "{delegated}");
    }
}
```

**Give a tool-using worker `.default_max_turns(n)`.** `into_tool()` runs the worker as a bare
`agent.prompt(prompt)` with the caller's tool context, so nothing set on the manager's run
reaches it: its turn budget is its own default, one model call unless its builder says
otherwise. Without the line above the worker fails every delegation with `MaxTurns`, the
manager reads only a tool result saying `the tool failed`, and it answers anyway. Budget the
manager as well: each delegation costs it a model call on top of the worker's whole run.

**Leave `.memory(..)` off a worker.** The same bare prompt names no conversation, so a worker
with memory loads and saves the one log named after it on every delegation, whichever user the
manager is serving. Keep the conversation on the manager, scoped per request.

**Always set `name` and `description` on an agent used as a tool.** `into_tool()` uses the
agent's name as the tool name and its description as the tool description. An unnamed
worker is still registered, but under the generic name `agent_tool` — so a manager with two
unnamed workers cannot tell them apart, and a manager with one cannot tell what it is for.

The generated tool description is a template built from the worker's name, description
**and preamble**, so delegating a sub-agent publishes its entire system prompt in the
manager's advertised tool list — which the model sees, and which anything reading that list
sees too. Keep a worker's preamble free of anything you would not put in front of the
manager's caller.

### Swarms

Rig ships no swarm primitive. Build one with the actor pattern: each agent owns a Tokio
task, an `mpsc` inbox, and senders to its peers, with `tokio::select!` reacting to either
an inbound message or a periodic self-check. Nothing in that is Rig-specific — each
participant just builds an agent and prompts it. For supervision and clustering, put
`ractor` underneath rather than hand-rolling it.

Bound every autonomous loop. An agent that can enqueue work for itself and for its peers
will happily run until your provider bill notices.

## Loaders

Loaders read files and turn them into text for agent context or embedding. They handle
globs, directory traversal, and per-file errors so ingestion stays fault-tolerant.

```rust
use rig::loaders::FileLoader;

// Glob, keeping each file's path
let examples = FileLoader::with_glob("examples/*.rs")?
    .read_with_path()   // yields (PathBuf, String)
    .ignore_errors()
    .into_iter();

// Whole directory
let files = FileLoader::with_dir("data/")?.read().ignore_errors();
```

Fold them into an agent's context:

```rust
use rig::agent::AgentBuilder;

let agent = examples
    .fold(AgentBuilder::new(model), |builder, (path, content)| {
        builder.context(&format!("Rust Example {path:?}:\n{content}"))
    })
    .build();
```

`PdfFileLoader` (feature `pdf`) and `EpubFileLoader` (feature `epub`) mirror the API;
`by_page()` iterates PDF pages individually, which is usually the right chunk boundary for
embedding.

`ignore_errors()` is what makes a loader usable over a real directory — without it one
unreadable file aborts the batch. Log what you skipped rather than discarding it silently.

**Filter the glob; never point `with_dir` at a repository root.** A directory sweep picks
up `.env`, key files, and `.git` contents and ships them to your model provider. Name the
extensions and subdirectories you actually want. Whatever survives the filter is untrusted
input once it is in the preamble — a comment in a source file is now instructions-adjacent,
so never let file content trigger an action you would not take on an anonymous user's
say-so.

## Interactive REPL

`ChatBotBuilder` turns an `Agent` into a terminal REPL — useful for trying an agent
locally without writing an input loop.

```rust,verify
use rig::integrations::cli_chatbot::ChatBotBuilder;
use rig::prelude::*;
use rig::providers::openai::{self, OpenAI};
const MODEL: &str = openai::GPT_6_SOL;


#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let agent = AgentBuilder::new(OpenAI::from_env()?.completion(MODEL))
        .preamble("You are a helpful assistant.")
        .build();

    ChatBotBuilder::new().agent(agent).build().run().await?;
    Ok(())
}
```

You get a `>` prompt, conversation history maintained across turns, an `exit` command, and
tracing spans when a subscriber is installed; the first failed prompt ends `run()` with its
error. It streams each answer with a one-call budget that overrides the agent's
`default_max_turns`, so a tool-using agent needs `.max_turns(n)` before `.build()`. It takes
any `Agent`, so a RAG agent with `dynamic_context` works unchanged;
`.chat(..)` in place of `.agent(..)` takes anything else that implements its `Chat` trait.

`ChatBotBuilder` needs a fully built `Agent`, not a builder.

Back to the reference index in SKILL.md.
