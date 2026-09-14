# RAG and Embeddings

Read this file when the user wants an agent grounded in their own documents, semantic
search, or tool-RAG for a large tool inventory.

Verified against `rig` 0.42.0.

`MODEL` in a snippet is the model-id binding described under Model ids in SKILL.md.

## Contents

- [The Two Phases](#the-two-phases)
- [Embeddings](#embeddings)
- [A Minimal RAG Agent](#a-minimal-rag-agent)
- [Retrieving Documents Yourself](#retrieving-documents-yourself)
- [Vector Stores](#vector-stores)
- [Tool-RAG](#tool-rag)
- [Beyond Top-K](#beyond-top-k)
- [Limitations to Design Around](#limitations-to-design-around)

## The Two Phases

**Ingestion.** Split documents into chunks (fixed token sizes of ~512–1000, or semantic
boundaries like paragraphs), embed each chunk, and insert the embeddings with their
metadata into a vector store.

**Retrieval.** Embed the user's query **with the same model**, run a similarity search, and
put the top results into the prompt. Vectors from different models are not comparable, and
mismatching the ingestion and query models fails silently: the search still returns its top
*n* results, they are just unrelated to the question. Nothing errors, so this is worth
checking first when retrieval quality looks random.

## Embeddings

```rust
use rig::embeddings::EmbeddingsBuilder;
use rig::prelude::*;
use rig::providers::openai;

let client = openai::Client::from_env()?;
let model = client.embedding_model("text-embedding-3-small");

let embeddings = EmbeddingsBuilder::new(model)
    .document("Some text".to_string())?
    .document("More text".to_string())?
    .build()
    .await?;
```

For a batch, use `.documents(..)` — it respects the provider's max batch size and handles
concurrency for you:

```rust
let embeddings = EmbeddingsBuilder::new(model)
    .documents(docs)?
    .build()
    .await?;
```

`build()` returns `Result<Vec<(T, Vec<Embedding>)>, EmbeddingError>`, where `T` is your
source type. The order matches the order you added documents, which is what lets
`add_documents` pair each vector with the right record.

### The `Embed` trait

To embed richer types than strings, implement `Embed` — it tells Rig which fields become
vectors. Derive it (needs the `derive` feature, on by default) and mark the fields:

```rust,verify
use rig::Embed;

#[derive(Embed, Clone)]
struct Article {
    id: i32,
    title: String,
    #[embed]
    body: String,
}
```

Or implement it by hand for full control:

```rust,verify
use rig::embeddings::{Embed, EmbedError, TextEmbedder};

struct WordDefinition {
    id: i32,
    word: String,
    definition: String,
}

impl Embed for WordDefinition {
    fn embed(&self, embedder: &mut TextEmbedder) -> Result<(), EmbedError> {
        // Only the definition carries meaning worth searching.
        embedder.embed(self.definition.clone());
        Ok(())
    }
}
```

Embed the field that answers the question, not every field. Embedding an id or a timestamp
adds noise to the vector and degrades retrieval.

## A Minimal RAG Agent

```rust,verify
use rig::embeddings::EmbeddingsBuilder;
use rig::prelude::*;
use rig::providers::openai;
use rig::vector_store::in_memory_store::InMemoryVectorStore;
const MODEL: &str = openai::GPT_5_5;


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = openai::Client::from_env()?;
    let embed_model = client.embedding_model("text-embedding-3-small");

    let embeddings = EmbeddingsBuilder::new(embed_model.clone())
        .documents(vec![
            "Rig is a Rust library for building LLM-powered applications.".to_string(),
            "RAG combines retrieval and generation for better accuracy.".to_string(),
            "Vector stores enable semantic search over documents.".to_string(),
        ])?
        .build()
        .await?;

    let mut store = InMemoryVectorStore::default();
    store.add_documents(embeddings);
    let index = store.index(embed_model);

    let agent = client
        .agent(MODEL)
        .preamble("Answer questions using the provided context. Say so if it is not covered.")
        .dynamic_context(2, index) // top 2 documents per model call
        .build();

    let answer = agent.prompt("What is Rig?").await?;
    println!("{answer}");

    Ok(())
}
```

`dynamic_context(n, index)` is a completion-call hook under the hood. It searches with the
prompt's first text part, falling back to the latest textual history message, and appends
the retrieved documents to the request **after** static context. Retrieval failure stops
the run before any provider I/O — a broken store fails loudly rather than silently
answering ungrounded.

Because it is a hook, it composes with your own hooks in registration order: register a
stop policy *before* `dynamic_context` if it should prevent retrieval from happening.

## Retrieving Documents Yourself

When you want the documents rather than an agent injecting them:

```rust
use rig::vector_store::{VectorSearchRequest, VectorStoreIndex};

let req = VectorSearchRequest::builder()
    .query("What is Rig?")
    .samples(2)
    .build();

let results = index.top_n::<String>(req).await?;
for (score, id, doc) in results {
    println!("score={score} id={id} doc={doc}");
}
```

`top_n::<T>` returns `Vec<(f64, String, T)>` — score, id, deserialized document.
`top_n_ids` returns just `(score, id)` when you only need to look something up elsewhere.
The request builder also accepts `.threshold(f64)` to drop weak matches and `.filter(..)`
for backend-specific metadata filtering.

A threshold is worth setting: without one, the "top 2" of an irrelevant corpus are still
returned, and the model dutifully answers from them.

## Vector Stores

`InMemoryVectorStore` ships with `rig` and is right for development, tests, and small
read-mostly corpora. Its constructors:

| Constructor | Use |
|---|---|
| `InMemoryVectorStore::default()` + `add_documents(..)` | Ordinary case |
| `from_documents(..)` | Build from an iterator in one step |
| `from_documents_with_ids(..)` | You already have stable ids |
| `from_documents_with_id_f(..)` | Derive the id from the document — the tool-RAG pattern |
| `builder()` | Configure the index strategy instead of the default brute-force scan |

Mind the bounds on the document type `D`: `Default` is required for `default()`, and
`Serialize + Eq` for `builder()` and the `from_documents*` family. A document type that
derives neither will fail to resolve the constructor rather than fail at the call site.

`store.index(embedding_model)` turns a store into a searchable `VectorStoreIndex`.

Durable stores are feature-gated on the `rig` facade crate: `lancedb`, `qdrant`,
`mongodb`, `postgres`, `sqlite`, `neo4j`, `surrealdb`, `milvus`, `s3vectors`, `scylladb`,
`helixdb`, `vectorize`.
They all implement the same `VectorStoreIndex` trait, so swapping the store does not change
the agent code:

```toml
rig = { version = "0.42", features = ["qdrant"] }
```

## Tool-RAG

A large tool inventory wastes context and degrades selection quality. Tool-RAG stores tool
definitions in a vector store and retrieves only the relevant ones per request.

A retrievable tool implements `ToolEmbedding` in addition to `Tool`:

```rust
use rig::tool::ToolEmbedding;

impl ToolEmbedding for Adder {
    // Reconstruction cannot fail here, so borrow std's uninhabited error type
    // instead of hand-rolling one.
    type InitError = std::convert::Infallible;
    type Context = ();
    type State = ();

    fn embedding_docs(&self) -> Vec<String> {
        vec!["Add two numbers together. Use for sums and totals.".into()]
    }

    fn context(&self) -> Self::Context {}

    fn init(_state: Self::State, _context: Self::Context) -> Result<Self, Self::InitError> {
        Ok(Adder)
    }
}
```

`embedding_docs` is what gets embedded and matched against the user's query — write it as
the question a user would ask, not as an API description. `Context` is serializable state
persisted alongside the vector; `init` reconstructs the tool from it.

Wire it up:

```rust
use rig::tool::ToolSet;

let mut toolset = ToolSet::default();
toolset.add_retrieved_tool(Adder);

let embeddings = EmbeddingsBuilder::new(embed_model.clone())
    .documents(toolset.schemas()?)?
    .build()
    .await?;

let store = InMemoryVectorStore::from_documents_with_id_f(embeddings, |tool| tool.name.clone());
let index = store.index(embed_model);

let agent = client
    .agent(MODEL)
    .preamble("You are a calculator. Use the tools provided.")
    .retrieved_tools(2, index, toolset)
    .build();
```

`retrieved_tools(n, index, toolset)` fetches the `n` most relevant tool definitions per
request and offers only those to the model; called tools are executed from the toolset.

Note the method name: in 0.42 `dynamic_tools(..)` means *runtime-defined* tools, not
retrieved ones. Website snippets using `dynamic_tools(2, index, toolset)` will not compile.

## Beyond Top-K

**Re-ranking.** Vector search compares two vectors that were produced independently, so it
never sees the query and the document together. A re-ranking model does: it scores the pair
jointly, which catches relevance that separate embeddings miss. Rig has first-class
re-ranking with no feature flag: `RerankModel`, `RerankResponse` and `RerankResult` are in
`rig::rerank`, while the client trait that builds one is `rig::client::RerankingClient` —
not in `rig::rerank`, which is the import that trips people up. Retrieve a generous top-k,
rerank, pass only the survivors to the model.

**Hybrid search.** Semantic search misses exact terms — product codes, error numbers,
proper nouns — because an embedding of a serial number is not meaningfully near the query's.
Store documents in a full-text index as well, query both, and merge with Reciprocal Rank
Fusion or weighted scoring. Rig has no hybrid-search abstraction; the second index and the
merge are yours to write.

**RAG as memory.** The same machinery stores conversation summaries and user facts for
retrieval by relevance. See Memory and History.

## Limitations to Design Around

- **Split context.** Relevant information spans chunks. Use 10–20% overlap, or
  parent-child chunking: retrieve small chunks, pass the larger parent to the model.
- **Contradictory sources.** Filter by metadata, weight by recency, and weight by source
  authority — prefer official documentation over forum posts.
- **Stale data.** Track `created_at` / `last_updated`, version your embeddings, and
  re-embed when source data changes. An embedding is a snapshot; nothing invalidates it for
  you.
- **Retrieved documents are untrusted input.** They land in the model's context and can
  carry instructions. Never let a retrieved document trigger an action your agent would not
  take from an anonymous user, and keep tool authorization out of the prompt layer.
- **Do you even need RAG?** For classification over well-known categories, better prompting
  is cheaper and more reliable. RAG earns its complexity when answers must be grounded in
  documents you own and update.

Back to the reference index in SKILL.md.
