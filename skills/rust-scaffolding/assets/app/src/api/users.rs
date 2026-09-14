//! One resource: request schema, response schema, handlers and router together.
//! Copy this file to start a second resource.
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use sea_orm::{ActiveModelTrait as _, ActiveValue::Set, EntityTrait as _, PaginatorTrait as _};
use sea_orm::{QueryOrder as _, QuerySelect as _};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use validator::Validate;

use crate::AppState;
use crate::entities::user;
use crate::error::{AppError, ErrorBody};
use crate::extract::{Valid, ValidQuery};

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

impl From<user::Model> for UserResponse {
    fn from(model: user::Model) -> Self {
        Self {
            id: model.id,
            email: model.email,
            name: model.name,
            created_at: model.created_at.into(),
        }
    }
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
        (status = 201, body = UserResponse),
        (status = 409, body = ErrorBody),
        (status = 422, body = ErrorBody),
    )
)]
// House style: `skip_all` plus the fields worth having. `skip(state)` would still
// record every other argument, which is how a secret ends up in a log line.
#[tracing::instrument(skip_all)]
pub async fn create_user(
    State(state): State<AppState>,
    // Extractors that consume the body go LAST in the argument list.
    Valid(body): Valid<CreateUser>,
) -> Result<(StatusCode, Json<UserResponse>), AppError> {
    let created = user::ActiveModel {
        id: Set(Uuid::now_v7()),
        email: Set(body.email),
        name: Set(body.name),
        created_at: Set(state.clock.now().into()),
    }
    // A struct literal plus `.insert()`. Never `.save()` a row whose primary key
    // the application set: `save()` reads a `Set` key as "update" and fails with
    // `RecordNotUpdated`.
    .insert(&state.db)
    .await?;

    metrics::counter!("users_created_total").increment(1);
    Ok((StatusCode::CREATED, Json(created.into())))
}

/// Fetch one user. Note `{id}`, not axum 0.7's `:id`.
///
/// # Errors
/// 404 when no user has that id.
#[utoipa::path(
    get, path = "/users/{id}", tag = "users",
    params(("id" = Uuid, Path, description = "User id")),
    responses((status = 200, body = UserResponse), (status = 404, body = ErrorBody))
)]
#[tracing::instrument(skip_all, fields(user_id = %id))]
pub async fn get_user(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<UserResponse>, AppError> {
    let found = user::Entity::find_by_id(id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("user {id} not found")))?;
    Ok(Json(found.into()))
}

/// List users, newest first.
///
/// # Errors
/// 422 when `limit` is outside 1..=100.
#[utoipa::path(
    get, path = "/users", tag = "users",
    params(ListUsers),
    responses((status = 200, body = Page<UserResponse>), (status = 422, body = ErrorBody))
)]
#[tracing::instrument(skip_all, fields(limit = query.limit, offset = query.offset))]
pub async fn list_users(
    State(state): State<AppState>,
    ValidQuery(query): ValidQuery<ListUsers>,
) -> Result<Json<Page<UserResponse>>, AppError> {
    let total = user::Entity::find().count(&state.db).await?;
    let items = user::Entity::find()
        // A second ordering key: OFFSET over a non-unique sort is not stable.
        .order_by_desc(user::COLUMN.created_at)
        .order_by_desc(user::COLUMN.id)
        .limit(query.limit)
        .offset(query.offset)
        .all(&state.db)
        .await?
        .into_iter()
        .map(UserResponse::from)
        .collect();

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
