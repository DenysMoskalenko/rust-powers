# OpenAPI with utoipa 6

- [One resource file](#one-resource-file)
- [Wiring the document](#wiring-the-document)
- [Streaming responses and OpenAPI 3.2](#streaming-responses-and-openapi-32)
- [What the generated document does not contain](#what-the-generated-document-does-not-contain)

## One resource file

A resource file holds its DTOs, its handlers and its router. `routes!(a, b)` groups handlers that
share a path into one `MethodRouter` and registers their `#[utoipa::path]` metadata.

```rust,verify
//! `api/users.rs` — one resource: request schema, response schema, handlers and
//! router together. Copy this file to start a second resource.
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use validator::Validate;

use crate::AppState;
use crate::error::{AppError, ErrorBody};
use crate::extract::{Path, Valid, ValidQuery};

#[derive(Debug, Deserialize, Validate, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUser {
    // Every `validate` rule is repeated as a `schema` constraint so Swagger shows it.
    #[validate(email)]
    #[schema(format = Email, example = "ada@example.com")]
    pub email: String,
    #[validate(length(min = 1, max = 100))]
    #[schema(min_length = 1, max_length = 100, example = "Ada Lovelace")]
    pub name: String,
}

/// The response DTO. Separate from the entity on purpose: an entity field is a
/// storage decision, a response field is a public contract.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// The pagination envelope every list endpoint returns.
#[derive(Debug, Serialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub limit: u64,
    pub offset: u64,
}

#[derive(Debug, Deserialize, Validate, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListUsers {
    // The serde default is what makes the parameter optional; `param(default)`
    // alone leaves it `required: true` in the document.
    #[validate(range(min = 1, max = 100))]
    #[serde(default = "default_limit")]
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub limit: u64,
    #[serde(default)]
    #[param(default = 0)]
    pub offset: u64,
}

fn default_limit() -> u64 {
    20
}

/// Create a user.
///
/// # Errors
/// 422 when the body fails validation, 409 when the email is taken.
#[utoipa::path(
    post, path = "/users", tag = "users",
    request_body = CreateUser,
    responses(
        (status = 201, body = UserResponse, description = "The created user"),
        (status = 409, body = ErrorBody, description = "The email is taken"),
        (status = 422, body = ErrorBody, description = "The body failed validation"),
    )
)]
// House style: `skip_all` plus the fields worth having. `skip(state)` would still
// record every other argument, which is how a secret ends up in a log line. No
// field here: the only identifier the request carries is the email, and that is
// PII, never a log field; the id exists once the row does.
#[tracing::instrument(skip_all)]
pub async fn create_user(
    State(state): State<AppState>,
    // Extractors that consume the body go LAST in the argument list.
    Valid(body): Valid<CreateUser>,
) -> Result<(StatusCode, Json<UserResponse>), AppError> {
    let user = create(&state, body).await?;
    metrics::counter!("users_created_total").increment(1);
    Ok((StatusCode::CREATED, Json(user)))
}

/// Fetch one user. Note `{id}`, not axum 0.7's `:id`.
///
/// # Errors
/// 404 when no user has that id.
#[utoipa::path(
    get, path = "/users/{id}", tag = "users",
    params(("id" = Uuid, Path, description = "User id")),
    responses(
        (status = 200, body = UserResponse, description = "The user"),
        (status = 404, body = ErrorBody, description = "No user has that id"),
    ),
    security(("bearer" = []))
)]
#[tracing::instrument(skip_all, fields(user_id = %id))]
pub async fn get_user(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<UserResponse>, AppError> {
    let user = fetch(&state, id).await?;
    Ok(Json(user))
}

/// List users, newest first.
///
/// # Errors
/// 422 when `limit` is outside 1..=100.
#[utoipa::path(
    get, path = "/users", tag = "users",
    params(ListUsers),
    responses(
        (status = 200, body = Page<UserResponse>, description = "One page of users"),
        (status = 422, body = ErrorBody, description = "`limit` is outside 1..=100"),
    )
)]
#[tracing::instrument(skip_all, fields(limit = query.limit, offset = query.offset))]
pub async fn list_users(
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<ListUsers>,
) -> Result<Json<Page<UserResponse>>, AppError> {
    let (items, total) = list(&state, query.limit, query.offset).await?;
    Ok(Json(Page {
        items,
        total,
        limit: query.limit,
        offset: query.offset,
    }))
}

pub fn router() -> OpenApiRouter<AppState> {
    // `routes!` groups handlers that share a path into one `MethodRouter`.
    OpenApiRouter::new()
        .routes(routes!(create_user, list_users))
        .routes(routes!(get_user))
}

// The service functions the handlers delegate to. The `ping` stands in for the
// query; see `sea-orm-postgres` for the real ones.
async fn create(state: &AppState, body: CreateUser) -> Result<UserResponse, AppError> {
    state.db.ping().await?;
    Ok(UserResponse {
        id: Uuid::now_v7(),
        email: body.email,
        name: body.name,
        created_at: state.clock.now(),
    })
}
async fn fetch(state: &AppState, id: Uuid) -> Result<UserResponse, AppError> {
    state.db.ping().await?;
    Err(AppError::NotFound(format!("user {id} not found")))
}
async fn list(
    state: &AppState,
    _limit: u64,
    _offset: u64,
) -> Result<(Vec<UserResponse>, u64), AppError> {
    state.db.ping().await?;
    Ok((Vec::new(), 0))
}
```

The scaffold's `users.rs` is this file with the three service functions replaced by one sea-orm
statement each, inline in the handler, and without `security(("bearer" = []))`: that attribute
references the scheme the `SecurityAddon` modifier below registers, which the scaffold's `ApiDoc`
does not have until `auth.md` adds it — copied alone, it documents an undefined scheme. `Page`
and `ListUsers` are the service-wide pagination contract.

## Wiring the document

```rust,verify
//! `lib.rs` — the document, the security scheme, and the split into router plus spec.
use axum::Router;
use utoipa::OpenApi;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use crate::AppState;

#[derive(OpenApi)]
#[openapi(
    info(title = "service", version = "0.1.0"),
    modifiers(&SecurityAddon),
    tags((name = "users", description = "User management"))
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // `components` is `None` until a schema is registered, so insert rather than unwrap.
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
    }
}

pub fn build_api(state: AppState) -> Router {
    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        // `OpenApiRouter::nest`, not `axum::Router::nest`: the axum version moves
        // the routes but leaves the document's paths unprefixed. The scaffold
        // has no prefix and `.merge(api::router())`s modules that declare
        // absolute paths, so the document and the router cannot drift.
        .nest("/api/v1", crate::api::users::router())
        .split_for_parts();

    router
        .merge(SwaggerUi::new("/docs").url("/openapi.json", api))
        .with_state(state)
}
```

`utoipa-swagger-ui` needs the `vendored` feature, or its build script downloads Swagger UI and
offline builds break. A route opts into the scheme with `security(("bearer" = []))`. The full
`build_router`, with the middleware stack and the `/metrics` route, is the scaffold's `lib.rs`;
this block shows only what the security scheme and a prefix add to it.

## Streaming responses and OpenAPI 3.2

OpenAPI 3.1 has no keyword for the items of a stream; 3.2 adds `itemSchema` for that. utoipa 6
writes `"openapi": "3.1.0"` unless the document opts in with `version = "3.2.0"`. Opt in only to
document an SSE or JSON Lines endpoint, and only once every consumer of the document (client
generator, gateway import, contract test) accepts 3.2. The bundled Swagger UI renders a 3.2
document, but shows only the media type, not the item schema.

```rust,verify
//! The SSE endpoint in the document. `ApiDoc` stands for the one in `lib.rs`, which gains
//! `version = "3.2.0"`; a second `ApiDoc` would stay unused.
use std::convert::Infallible;

use axum::response::sse::{Event, Sse};
use tokio_stream::Stream;
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;

#[derive(OpenApi)]
#[openapi(version = "3.2.0", info(title = "service", version = "0.1.0"))]
pub struct ApiDoc;

/// One event as the client parses it. For `text/event-stream`, `itemSchema` describes the
/// event's fields (`event`, `data`, `id`, `retry`), not the payload, and `data` is a string.
#[derive(ToSchema)]
pub struct JobEvent {
    /// `progress` for each step, then `error` once if the job fails.
    pub event: String,
    /// The step, as text.
    pub data: String,
}

#[utoipa::path(
    get, path = "/events", tag = "events",
    responses((
        status = 200, description = "Progress events until the job ends",
        content_type = "text/event-stream", item_schema = inline(JobEvent),
    )),
)]
pub async fn events() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    // A stub: keep the existing handler's signature and body.
    Sse::new(tokio_stream::iter([Ok(Event::default().event("progress").data("step 0"))]))
}

/// `routes!` on an `OpenApiRouter`: a plain `axum::Router::route` leaves the path out of the
/// document.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(events))
}
```

`item_schema` is written even into a 3.1.0 document, where `itemSchema` is not a valid keyword, so
add `version` in the same change. When `data` carries JSON, mark the field
`#[schema(content_media_type = "application/json")]`. The spec pairs that with `contentSchema` for
the payload's shape, which the derive cannot emit, so name the payload type in the field's doc
comment.

## What the generated document does not contain

| Expectation | Reality |
|---|---|
| `#[validate(range(min = 18))]` appears as `minimum` | It does not. utoipa never reads validator attributes. Repeat it as `#[schema(minimum = 18)]`. |
| `#[derive(ToSchema)]` puts a type in `components.schemas` | Only if a registered path references it, or it is listed in `#[openapi(components(schemas(Status)))]`. |
| `item_schema = JobEvent` registers `JobEvent` like `body` does | It does not, so the `$ref` dangles. `inline(JobEvent)` fixes only `JobEvent` itself; a field whose type derives `ToSchema` still dangles. Listing the type in `components(schemas(..))` registers it and the types it nests. |
| `#[param(default = 20)]` makes a query parameter optional | It stays `required: true`. Optional means `Option<T>` or `#[serde(default)]` on the field. |
| `u8` with a `max` rule is bounded | The emitted `minimum: 0` comes from unsignedness; there is no `maximum`. |
| `Option<String>` is `nullable: true` | OpenAPI 3.1 style: `"type": ["string", "null"]`. |
| A response without `description` is still valid | utoipa 6 omits an empty `description` yet still declares `"openapi": "3.1.0"`, which requires one per response, so the document is invalid 3.1. Give every `(status = ..)` a `description`. |

What utoipa does read: serde attributes. `deny_unknown_fields` becomes
`"additionalProperties": false` and `rename_all` renames the properties. Enum handling follows
serde too — a C-like enum becomes a string enum and `#[serde(tag = "type")]` becomes a `oneOf`
whose variants each carry `type` as a one-value enum, with no `discriminator` object. A generic
response type is named after its parameter, so `Page<UserResponse>` appears as `Page_UserResponse`.

External types need their utoipa feature (`chrono`, `uuid`, `decimal`); without it the derive
fails because the type has no `ToSchema` impl. `axum_extras` is what makes `IntoParams` inference
work for axum extractors.
