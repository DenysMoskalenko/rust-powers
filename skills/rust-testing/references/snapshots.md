# Snapshots and diff assertions

## When a snapshot is the right assertion

A snapshot asserts on a whole value at once. It pays off for a response body with many fields, a rendered error payload, or a serialisation format that must not drift. It is the wrong tool for a single field, for anything an explicit `assert_eq!` states more clearly, and for a value nobody would notice changing — an accepted-without-reading snapshot asserts nothing.

Every value that varies per run must be redacted, or the snapshot fails on the next run.

## insta with redactions

```rust,verify,test
#![allow(clippy::unwrap_used, reason = "test helpers")]

mod common;

use axum::http::StatusCode;
use insta::assert_json_snapshot;
use serde_json::json;

#[tokio::test]
async fn create_user_response_shape() {
    let app = common::test_app().await;

    let response = app
        .server
        .post("/users")
        .json(&json!({ "email": "ada@example.com", "name": "Ada" }))
        .await;

    response.assert_status(StatusCode::CREATED);
    // One redaction, for the one value that changes every run. `created_at`
    // needs none: the fixed clock already pins it, so the snapshot proves it.
    assert_json_snapshot!(response.json::<serde_json::Value>(), {
        ".id" => "[uuid]",
    });
}
```

That writes `tests/snapshots/<binary>__create_user_response_shape.snap`, which is committed and reviewed like any other file:

```text
---
source: tests/users.rs
expression: "response.json::<serde_json::Value>()"
---
{
  "created_at": "2026-09-13T12:00:00Z",
  "email": "ada@example.com",
  "id": "[uuid]",
  "name": "Ada"
}
```

Redact only what genuinely varies. A `created_at` pinned by `FixedClock` stays in the snapshot, where it keeps proving the timestamp came from the injected clock; redacting it would throw that assertion away.

## Selector syntax

| Selector | Matches |
|---|---|
| `.id` | a top-level field |
| `.user.email` | a nested field |
| `.items[0].id` | one element |
| `.items[].id` | every element of a list |
| `.**.id` | every `id` at any depth |

Macros: `assert_json_snapshot!`, `assert_yaml_snapshot!`, `assert_debug_snapshot!` (Debug), `assert_snapshot!` (Display). Redactions need the `json` and `redactions` features.

## Review workflow

| Command | Effect |
|---|---|
| `cargo insta test --test-runner nextest` | run, writing `.snap.new` for changes |
| `cargo insta review` | interactive accept or reject, one snapshot at a time |
| `cargo insta accept` | accept everything pending — only after reading the diff |
| `cargo insta test --unreferenced=reject` | fail when a `.snap` file no longer belongs to any test |

The first run of a new snapshot always fails: `INSTA_UPDATE` defaults to `auto`, which locally writes a `.snap.new` to review and in CI writes nothing. Create the snapshot with `cargo insta test --accept`, then commit it. Never set `INSTA_UPDATE=always` in CI: it turns every snapshot test into a no-op that rewrites the expectation it was meant to guard.

## similar-asserts

```rust
similar_asserts::assert_eq!(actual, expected);
```

A drop-in replacement for `assert_eq!` that prints a structural, coloured diff. Reach for it whenever the failure message would otherwise be two large values printed end to end. Keep one assertion style per project: `assert_eq!` for scalars, `similar_asserts::assert_eq!` for anything whose diff has to be read, a snapshot for a whole body, and no `assert!(a == b)` anywhere.
