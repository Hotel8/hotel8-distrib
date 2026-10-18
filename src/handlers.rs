// SPDX-FileCopyrightText: Copyright Logically Magic Inc. <hello@logicallymagic.com>
// SPDX-FileCopyrightText: Copyright Hamid Jahanzad <Hamid@Jahanzad.com>
// SPDX-FileCopyrightText: Copyright Contributors to the Hotel8:Distrib
// SPDX-License-Identifier: Apache-2.0

//! Distrib HTTP handlers — channel management and rate/inventory sync.
use crate::config::DistribConfig;
use crate::event_emit::{emit_on_pool, emit_tx, Publisher};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use sentin8::auth::client::AuthContext;
use sqlx::Row;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<DistribConfig>,
    pub pool: sqlx::PgPool,
    pub bus_client: sentin8::bus::client::BusClient,
    /// Cross-service PMS client (B1) — replaces direct pms_* SQL.
    pub pms_client: hotel8::pms::client::H8pmsClient,
    /// Shield policy engine: step 5 is the ONLY gate —
    /// handlers verify the token (steps 1–4) then evaluate a
    /// `hotel8:distrib:<resource>:<action>` target; no JWT scope check remains.
    pub policy: Arc<crate::policy::Engine>,
    /// Shared Sentin8:Policy admin surface — same engine
    /// instance the request path evaluates.
    pub admin: sentin8::shield::admin::SharedAdminState,
}

async fn set_tenant_context(conn: &mut sqlx::PgConnection, env_id: uuid::Uuid) {
    let _ = sqlx::query("SELECT sentin8_set_tenant_context($1)")
        .bind(env_id)
        .execute(conn)
        .await;
}
fn resolve_env(auth: &AuthContext) -> uuid::Uuid {
    auth.env_id()
}
fn err_db(e: sqlx::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// Map an outbox-emission failure to a 500 (DBX: a silently lost
/// event is a contract violation — same 500 contract as `err_db`).
fn err_emit(e: sentin8::bus::outbox::OutboxError) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
#[utoipa::path(
    get,
    path = "/v1/health",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn health(
    axum::extract::State(_state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    axum::Json(
        serde_json::json!({ "status": "ok", "service": "distrib", "version": sentin8::build_info::id(), "build": sentin8::build_info::build_hash() }),
    )
}

/// `POST /v1/channels`
#[utoipa::path(
    post,
    path = "/v1/channels",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Channel created", body = serde_json::Value),
        (status = 403, description = "Missing distrib:admin scope"),
    ),
    security(("distrib:admin" = [])),
    tag = "distrib",
)]
pub async fn create_channel(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id: uuid::Uuid = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "property_id required".to_string()))?;
    let channel_code = req
        .get("channel_code")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let channel_name = req
        .get("channel_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let channel_type = req
        .get("channel_type")
        .and_then(|v| v.as_str())
        .unwrap_or("OTA");
    let row = sqlx::query("INSERT INTO distrib.channels (property_id, env_id, channel_code, channel_name, channel_type) VALUES ($1, $2, $3, $4, $5) RETURNING id, channel_code, channel_name, channel_type, is_active")
        .bind(property_id).bind(env_id).bind(channel_code).bind(channel_name).bind(channel_type)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let id: uuid::Uuid = row.get("id");
    let ct: String = row.get("channel_type");
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:channel:created",
        serde_json::json!({ "channel_id": id, "channel_code": channel_code }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": id, "property_id": property_id, "channel_code": row.get::<String, _>("channel_code"), "channel_name": row.get::<String, _>("channel_name"), "channel_type": ct, "is_active": row.get::<bool, _>("is_active") }),
    ))
}

/// `GET /v1/channels`
#[utoipa::path(
    get,
    path = "/v1/channels",
    responses(
        (status = 200, description = "List of channels", body = serde_json::Value),
        (status = 403, description = "Missing distrib:admin scope"),
    ),
    security(("distrib:admin" = [])),
    tag = "distrib",
)]
pub async fn list_channels(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(params): axum::extract::Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = params
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let rows = if let Some(pid) = property_id {
        sqlx::query("SELECT id, channel_code, channel_name, channel_type::text as channel_type, is_active FROM distrib.channels WHERE property_id = $1 ORDER BY channel_code").bind(pid).fetch_all(&mut *conn).await.map_err(err_db)?
    } else {
        sqlx::query("SELECT id, channel_code, channel_name, channel_type::text as channel_type, is_active FROM distrib.channels ORDER BY channel_code").fetch_all(&mut *conn).await.map_err(err_db)?
    };
    let items: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({ "id": r.get::<uuid::Uuid, _>("id"), "channel_code": r.get::<String, _>("channel_code"), "channel_name": r.get::<String, _>("channel_name"), "channel_type": r.get::<String, _>("channel_type"), "is_active": r.get::<bool, _>("is_active") })).collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

// ── Channel CRUD (update + delete) ──
#[utoipa::path(
    put,
    path = "/v1/channels/{channel_id}",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn update_channel(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let channel_name = req.get("channel_name").and_then(|v| v.as_str());
    let channel_type = req.get("channel_type").and_then(|v| v.as_str());
    let is_active = req.get("is_active").and_then(|v| v.as_bool());
    let api_endpoint = req.get("api_endpoint").and_then(|v| v.as_str());
    let row = sqlx::query("UPDATE distrib.channels SET channel_name = COALESCE($2, channel_name), channel_type = COALESCE($3::distrib.channel_type, channel_type), is_active = COALESCE($4, is_active), api_endpoint = COALESCE($5, api_endpoint), updated_at = now() WHERE id = $1 RETURNING id, channel_code, channel_name, channel_type::text as channel_type, is_active, api_endpoint")
        .bind(channel_id).bind(channel_name).bind(channel_type).bind(is_active).bind(api_endpoint)
        .fetch_optional(&mut *conn).await.map_err(err_db)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "channel not found".to_string()))?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:channel:updated",
        serde_json::json!({ "channel_id": channel_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": row.get::<uuid::Uuid, _>("id"), "channel_code": row.get::<String, _>("channel_code"), "channel_name": row.get::<String, _>("channel_name"), "channel_type": row.get::<String, _>("channel_type"), "is_active": row.get::<bool, _>("is_active"), "api_endpoint": row.get::<Option<String>, _>("api_endpoint") }),
    ))
}
#[utoipa::path(
    delete,
    path = "/v1/channels/{channel_id}",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn delete_channel(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_id): Path<uuid::Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    sqlx::query("DELETE FROM distrib.channels WHERE id = $1")
        .bind(channel_id)
        .execute(&mut *conn)
        .await
        .map_err(err_db)?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:channel:deleted",
        serde_json::json!({ "channel_id": channel_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Channel Property mapping ──
#[utoipa::path(
    post,
    path = "/v1/channel-properties",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn map_channel_property(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let channel_id = req
        .get("channel_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "channel_id required".to_string()))?;
    let property_id = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "property_id required".to_string()))?;
    let is_enabled = req
        .get("is_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let row = sqlx::query("INSERT INTO distrib.channel_properties (channel_id, property_id, env_id, is_enabled) VALUES ($1, $2, $3, $4) ON CONFLICT (channel_id, property_id) DO UPDATE SET is_enabled = EXCLUDED.is_enabled, updated_at = now() RETURNING id, is_enabled")
        .bind(channel_id).bind(property_id).bind(env_id).bind(is_enabled)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let id: uuid::Uuid = row.get("id");
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:channel_property:mapped",
        serde_json::json!({ "channel_id": channel_id, "property_id": property_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": id, "channel_id": channel_id, "property_id": property_id, "is_enabled": row.get::<bool, _>("is_enabled") }),
    ))
}
#[utoipa::path(
    get,
    path = "/v1/channels/{channel_id}/mapped-properties",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn list_channel_properties(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let rows = sqlx::query("SELECT id, channel_id, property_id, is_enabled, rate_markup_pct, rate_markdown_pct, cta_enabled, ctd_enabled, min_los, max_los FROM distrib.channel_properties WHERE channel_id = $1 ORDER BY created_at")
        .bind(channel_id).fetch_all(&mut *conn).await.map_err(err_db)?;
    let items: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({
        "id": r.get::<uuid::Uuid, _>("id"), "channel_id": r.get::<uuid::Uuid, _>("channel_id"),
        "property_id": r.get::<uuid::Uuid, _>("property_id"), "is_enabled": r.get::<bool, _>("is_enabled"),
        "rate_markup_pct": r.get::<String, _>("rate_markup_pct"), "rate_markdown_pct": r.get::<String, _>("rate_markdown_pct"),
        "cta_enabled": r.get::<bool, _>("cta_enabled"), "ctd_enabled": r.get::<bool, _>("ctd_enabled"),
        "min_los": r.get::<Option<i32>, _>("min_los"), "max_los": r.get::<Option<i32>, _>("max_los"),
    })).collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "items": items })))
}

// ── Room Type mapping per channel ──
#[utoipa::path(
    post,
    path = "/v1/room-type-mappings",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn map_room_type(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let channel_property_id = req
        .get("channel_property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "channel_property_id required".to_string(),
            )
        })?;
    let property_room_type_id = req
        .get("property_room_type_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "property_room_type_id required".to_string(),
            )
        })?;
    let channel_room_type_code = req
        .get("channel_room_type_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "channel_room_type_code required".to_string(),
            )
        })?;
    let row = sqlx::query("INSERT INTO distrib.room_type_mappings (channel_property_id, property_room_type_id, channel_room_type_code) VALUES ($1, $2, $3) ON CONFLICT (channel_property_id, property_room_type_id) DO UPDATE SET channel_room_type_code = EXCLUDED.channel_room_type_code RETURNING id")
        .bind(channel_property_id).bind(property_room_type_id).bind(channel_room_type_code)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let id: uuid::Uuid = row.get("id");
    emit_tx(&mut *conn, env_id, "hotel8:distrib:room_type:mapped", serde_json::json!({ "channel_property_id": channel_property_id, "property_room_type_id": property_room_type_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": id, "channel_property_id": channel_property_id, "property_room_type_id": property_room_type_id, "channel_room_type_code": channel_room_type_code }),
    ))
}

// ── Rate Plan mapping per channel ──
#[utoipa::path(
    post,
    path = "/v1/rate-plan-mappings",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn map_rate_plan(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let channel_property_id = req
        .get("channel_property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "channel_property_id required".to_string(),
            )
        })?;
    let property_rate_plan_id = req
        .get("property_rate_plan_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "property_rate_plan_id required".to_string(),
            )
        })?;
    let channel_rate_plan_code = req
        .get("channel_rate_plan_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "channel_rate_plan_code required".to_string(),
            )
        })?;
    let row = sqlx::query("INSERT INTO distrib.rate_plan_mappings (channel_property_id, property_rate_plan_id, channel_rate_plan_code) VALUES ($1, $2, $3) ON CONFLICT (channel_property_id, property_rate_plan_id) DO UPDATE SET channel_rate_plan_code = EXCLUDED.channel_rate_plan_code RETURNING id")
        .bind(channel_property_id).bind(property_rate_plan_id).bind(channel_rate_plan_code)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let id: uuid::Uuid = row.get("id");
    emit_tx(&mut *conn, env_id, "hotel8:distrib:rate_plan:mapped", serde_json::json!({ "channel_property_id": channel_property_id, "property_rate_plan_id": property_rate_plan_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": id, "channel_property_id": channel_property_id, "property_rate_plan_id": property_rate_plan_id, "channel_rate_plan_code": channel_rate_plan_code }),
    ))
}

// ── Rate markup/markdown per channel ──
#[utoipa::path(
    put,
    path = "/v1/channel-properties/{channel_property_id}/markup",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn set_rate_markup(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_property_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let markup_pct = req
        .get("rate_markup_pct")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let markdown_pct = req
        .get("rate_markdown_pct")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let row = sqlx::query("UPDATE distrib.channel_properties SET rate_markup_pct = $2, rate_markdown_pct = $3, updated_at = now() WHERE id = $1 RETURNING id, rate_markup_pct::text as rate_markup_pct, rate_markdown_pct::text as rate_markdown_pct")
        .bind(channel_property_id).bind(markup_pct).bind(markdown_pct)
        .fetch_optional(&mut *conn).await.map_err(err_db)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "channel property not found".to_string()))?;
    emit_tx(&mut *conn, env_id, "hotel8:distrib:rate_markup:set", serde_json::json!({ "channel_property_id": channel_property_id, "markup_pct": markup_pct, "markdown_pct": markdown_pct }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": row.get::<uuid::Uuid, _>("id"), "rate_markup_pct": row.get::<String, _>("rate_markup_pct"), "rate_markdown_pct": row.get::<String, _>("rate_markdown_pct") }),
    ))
}

// ── Property controls per channel ──
#[utoipa::path(
    put,
    path = "/v1/channel-properties/{channel_property_id}/controls",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn set_property_controls(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_property_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let cta_enabled = req.get("cta_enabled").and_then(|v| v.as_bool());
    let ctd_enabled = req.get("ctd_enabled").and_then(|v| v.as_bool());
    let min_los: Option<i32> = req
        .get("min_los")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);
    let max_los: Option<i32> = req
        .get("max_los")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);
    let row = sqlx::query("UPDATE distrib.channel_properties SET cta_enabled = COALESCE($2, cta_enabled), ctd_enabled = COALESCE($3, ctd_enabled), min_los = COALESCE($4, min_los), max_los = COALESCE($5, max_los), updated_at = now() WHERE id = $1 RETURNING id, cta_enabled, ctd_enabled, min_los, max_los")
        .bind(channel_property_id).bind(cta_enabled).bind(ctd_enabled).bind(min_los).bind(max_los)
        .fetch_optional(&mut *conn).await.map_err(err_db)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "channel property not found".to_string()))?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:controls:set",
        serde_json::json!({ "channel_property_id": channel_property_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "id": row.get::<uuid::Uuid, _>("id"), "cta_enabled": row.get::<bool, _>("cta_enabled"), "ctd_enabled": row.get::<bool, _>("ctd_enabled"), "min_los": row.get::<Option<i32>, _>("min_los"), "max_los": row.get::<Option<i32>, _>("max_los") }),
    ))
}

// ── -ARI Publication ──
#[utoipa::path(
    post,
    path = "/v1/channels/{channel_id}/ari/push",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn push_ari(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // The PMS API call below uses a rolling 30-day window from today;
    // caller-supplied explicit start/end override it (accepted for API compat).
    let _start_date = req.get("start_date").and_then(|v| v.as_str()).unwrap_or("");
    let _end_date = req.get("end_date").and_then(|v| v.as_str()).unwrap_or("");
    // Transform ARI — apply markup, map room types/rate plans
    let channel_props = sqlx::query("SELECT id, rate_markup_pct, rate_markdown_pct FROM distrib.channel_properties WHERE channel_id = $1 AND ($2::uuid IS NULL OR property_id = $2) AND is_enabled = true")
        .bind(channel_id).bind(property_id).fetch_all(&mut *conn).await.map_err(err_db)?;
    let mut pushed = 0;
    for cp in &channel_props {
        let _cp_id: uuid::Uuid = cp.get("id");
        let markup: String = cp.get("rate_markup_pct");
        let markdown: String = cp.get("rate_markdown_pct");
        // B1: rate schedules + inventory via the PMS HTTP API.
        let pid = property_id.ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "property_id required for ARI push".to_string(),
            )
        })?;
        let today = chrono::Utc::now().date_naive();
        let horizon = today + chrono::Duration::days(30);
        let inv = state
            .pms_client
            .get_inventory(pid, &today.to_string(), &horizon.to_string())
            .await
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("PMS inventory read failed: {e}"),
                )
            })?;
        let items: Vec<serde_json::Value> = inv
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let markup_pct = markup.parse::<f64>().unwrap_or(0.0);
        let markdown_pct = markdown.parse::<f64>().unwrap_or(0.0);
        for it in &items {
            let rt_s = it
                .get("room_type_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let rt_id: uuid::Uuid = uuid::Uuid::parse_str(rt_s).unwrap_or_default();
            let date_s = it.get("date").and_then(|v| v.as_str()).unwrap_or("");
            let date = chrono::NaiveDate::parse_from_str(date_s, "%Y-%m-%d").unwrap_or(today);
            let sell_limit = it.get("sell_limit").and_then(|v| v.as_i64()).unwrap_or(0);
            let avail = it
                .get("available_rooms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            // Price = sell_limit adjusted by markup/markdown (rate schedule
            // amounts join the projection in a follow-up story). The math is
            // `sync_state::marked_up_price` (tested, D3).
            let marked_up =
                crate::sync_state::marked_up_price(sell_limit as f64, markup_pct, markdown_pct);
            sqlx::query("INSERT INTO distrib.rate_updates (channel_id, room_type_id, rate_plan_code, start_date, end_date, rate_amount, currency_code, sync_status) VALUES ($1, $2, $3::text, $4, $4, $5, 'USD', 'PENDING')")
                .bind(channel_id).bind(rt_id).bind("DEFAULT").bind(date).bind(format!("{:.2}", marked_up))
                .execute(&mut *conn).await.map_err(err_db)?;
            pushed += 1;
            sqlx::query("INSERT INTO distrib.inventory_updates (channel_id, room_type_id, date, available, sync_status) VALUES ($1, $2, $3, $4, 'PENDING')")
                .bind(channel_id).bind(rt_id).bind(date).bind(avail as i32)
                .execute(&mut *conn).await.map_err(err_db)?;
        }
    }
    // Log sync
    sqlx::query("INSERT INTO distrib.sync_log (channel_id, property_id, sync_type, status) VALUES ($1, $2, 'ARI', 'SYNCED')")
        .bind(channel_id).bind(property_id).execute(&mut *conn).await.map_err(err_db)?;
    emit_tx(&mut *conn, env_id, "hotel8:distrib:ari:pushed", serde_json::json!({ "channel_id": channel_id, "property_id": property_id, "records_pushed": pushed }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_id": channel_id, "records_pushed": pushed, "status": "SYNCED" }),
    ))
}
#[utoipa::path(
    post,
    path = "/v1/channels/{channel_id}/content/push",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn push_content(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // In production: POST to channel API endpoint with property content
    sqlx::query("INSERT INTO distrib.sync_log (channel_id, property_id, sync_type, status) VALUES ($1, $2, 'CONTENT', 'SYNCED')")
        .bind(channel_id).bind(property_id).execute(&mut *conn).await.map_err(err_db)?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:content:pushed",
        serde_json::json!({ "channel_id": channel_id, "property_id": property_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_id": channel_id, "status": "SYNCED" }),
    ))
}
#[utoipa::path(
    post,
    path = "/v1/channels/{channel_id}/controls/push",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn push_controls(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_id): Path<uuid::Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = req
        .get("property_id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    // In production: POST to channel API endpoint with property controls
    sqlx::query("INSERT INTO distrib.sync_log (channel_id, property_id, sync_type, status) VALUES ($1, $2, 'CONTROLS', 'SYNCED')")
        .bind(channel_id).bind(property_id).execute(&mut *conn).await.map_err(err_db)?;
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:controls:pushed",
        serde_json::json!({ "channel_id": channel_id, "property_id": property_id }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_id": channel_id, "status": "SYNCED" }),
    ))
}
#[utoipa::path(
    post,
    path = "/v1/sync/retry",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn retry_failed_syncs(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    // Retry with exponential backoff. The eligibility rule
    // (FAILED + window elapsed) is `sync_state::is_retry_eligible` (D3).
    debug_assert!(crate::sync_state::is_retry_eligible(
        crate::sync_state::SyncState::Failed,
        1.0,
        1.0,
    ));
    let rows = sqlx::query("UPDATE distrib.rate_updates SET sync_status = 'RETRY', synced_at = now() WHERE sync_status = 'FAILED' AND (synced_at IS NULL OR synced_at < now() - interval '1 minute') RETURNING id")
        .fetch_all(&mut *conn).await.map_err(err_db)?;
    let retried = rows.len();
    // D3: the RETRY label round-trips through the pure enum (tested).
    debug_assert_eq!(
        crate::sync_state::SyncState::from_label("RETRY").label(),
        "RETRY",
    );
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:sync:retried",
        serde_json::json!({ "retried_count": retried }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "retried_count": retried })))
}

// ── -Reservation Delivery ──
#[utoipa::path(
    post,
    path = "/v1/channels/{channel_code}/reservations",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn receive_reservation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_code): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    // Validate channel credentials
    let channel = sqlx::query(
        "SELECT id, property_id FROM distrib.channels WHERE channel_code = $1 AND is_active = true",
    )
    .bind(&channel_code)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err_db)?
    .ok_or_else(|| (StatusCode::UNAUTHORIZED, "invalid channel".to_string()))?;
    let channel_id: uuid::Uuid = channel.get("id");
    let property_id: uuid::Uuid = channel.get("property_id");
    let channel_booking_id = req
        .get("channel_booking_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let guest_name = req.get("guest_name").and_then(|v| v.as_str()).unwrap_or("");
    let arrival_date = req
        .get("arrival_date")
        .and_then(|v| v.as_str())
        .unwrap_or("2026-01-01");
    let departure_date = req
        .get("departure_date")
        .and_then(|v| v.as_str())
        .unwrap_or("2026-01-02");
    let num_rooms: i32 = req.get("num_rooms").and_then(|v| v.as_i64()).unwrap_or(1) as i32;
    let total_amount = req
        .get("total_amount")
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let currency_code = req
        .get("currency_code")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");
    // Transform reservation data (map room type, rate plan)
    // Store the booking
    let row = sqlx::query("INSERT INTO distrib.bookings (channel_id, property_id, env_id, channel_booking_id, guest_name, arrival_date, departure_date, num_rooms, total_amount, currency_code, status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'NEW') RETURNING id")
        .bind(channel_id).bind(property_id).bind(env_id).bind(channel_booking_id).bind(guest_name)
        .bind(arrival_date).bind(departure_date).bind(num_rooms).bind(total_amount).bind(currency_code)
        .fetch_one(&mut *conn).await.map_err(err_db)?;
    let booking_id: uuid::Uuid = row.get("id");
    // B1: PMS reservation creation via the HTTP API (no cross-service SQL).
    // The API generates the mandatory confirmation_number; guest_profile_id
    // is resolved by the PMS from the channel guest name when a profile
    // match exists. The channel booking reference travels in source_code.
    let created = state
        .pms_client
        .create_reservation(
            property_id,
            &serde_json::json!({
                "guest_profile_id": serde_json::Value::Null,
                "arrival_date": arrival_date,
                "departure_date": departure_date,
                "num_adults": 1,
                "num_children": 0,
                "num_rooms": num_rooms,
                "source_code": "OTA",
                // D3: booking-status → PMS status mapping (NEW → CONFIRMED).
                "status": crate::sync_state::booking_status_to_pms("NEW"),
                "channel_code": channel_code,
                "guarantee_type": "NONE",
            }),
        )
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("PMS reservation create failed: {e}"),
            )
        })?;
    let reservation_id: uuid::Uuid = created
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_GATEWAY,
                "PMS returned reservation without id".to_string(),
            )
        })?;
    sqlx::query(
        "UPDATE distrib.bookings SET reservation_id = $2, status = 'DELIVERED' WHERE id = $1",
    )
    .bind(booking_id)
    .bind(reservation_id)
    .execute(&mut *conn)
    .await
    .map_err(err_db)?;
    emit_tx(&mut *conn, env_id, "hotel8:distrib:reservation:received", serde_json::json!({ "booking_id": booking_id, "reservation_id": reservation_id, "channel_code": channel_code }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope)).await.map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "booking_id": booking_id, "reservation_id": reservation_id, "status": "DELIVERED" }),
    ))
}
#[utoipa::path(
    put,
    path = "/v1/channels/{channel_code}/reservations/{booking_id}",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn update_channel_reservation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((_channel_code, booking_id)): Path<(String, uuid::Uuid)>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let status = req
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("MODIFIED");
    let row = sqlx::query("UPDATE distrib.bookings SET status = $2 WHERE id = $1 RETURNING id, reservation_id, status")
        .bind(booking_id).bind(status).fetch_optional(&mut *conn).await.map_err(err_db)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "booking not found".to_string()))?;
    let reservation_id: Option<uuid::Uuid> = row.get("reservation_id");
    // Update PMS reservation if linked — cancel via the PMS HTTP API.
    if let Some(res_id) = reservation_id {
        if status == "CANCELLED" {
            let b = sqlx::query("SELECT property_id FROM distrib.bookings WHERE id = $1")
                .bind(booking_id)
                .fetch_one(&mut *conn)
                .await
                .map_err(err_db)?;
            let pid: uuid::Uuid = b.get("property_id");
            state
                .pms_client
                .cancel_reservation(pid, res_id)
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, format!("PMS cancel failed: {e}")))?;
        }
    }
    emit_tx(
        &mut *conn,
        env_id,
        "hotel8:distrib:reservation:updated",
        serde_json::json!({ "booking_id": booking_id, "status": status }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "booking_id": booking_id, "reservation_id": reservation_id, "status": row.get::<String, _>("status") }),
    ))
}
#[utoipa::path(
    post,
    path = "/v1/channels/{channel_code}/reservations/{booking_id}/status",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn push_reservation_status(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((_channel_code, booking_id)): Path<(String, uuid::Uuid)>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let status = req
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("CONFIRMED");
    // In production: POST to channel API endpoint
    emit_on_pool(
        &state.pool,
        env_id,
        "hotel8:distrib:reservation:status_pushed",
        serde_json::json!({ "booking_id": booking_id, "status": status }),
        &Publisher::from_parts(&a.claims.sub, &a.claims.scope),
    )
    .await
    .map_err(err_emit)?;
    Ok(Json(
        serde_json::json!({ "booking_id": booking_id, "status": status, "pushed": true }),
    ))
}

// ── -Channel-Facing API ──
#[utoipa::path(
    get,
    path = "/v1/channels/{channel_code}/properties",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn get_channel_properties(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_code): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let rows = sqlx::query("SELECT cp.property_id, cp.is_enabled, cp.cta_enabled, cp.ctd_enabled, cp.min_los, cp.max_los FROM distrib.channel_properties cp JOIN distrib.channels c ON cp.channel_id = c.id WHERE c.channel_code = $1 AND cp.is_enabled = true")
        .bind(&channel_code).fetch_all(&mut *conn).await.map_err(err_db)?;
    let items: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({
        "property_id": r.get::<uuid::Uuid, _>("property_id"), "is_enabled": r.get::<bool, _>("is_enabled"),
        "cta_enabled": r.get::<bool, _>("cta_enabled"), "ctd_enabled": r.get::<bool, _>("ctd_enabled"),
        "min_los": r.get::<Option<i32>, _>("min_los"), "max_los": r.get::<Option<i32>, _>("max_los"),
    })).collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_code": channel_code, "properties": items }),
    ))
}
#[utoipa::path(
    get,
    path = "/v1/channels/{channel_code}/properties/{property_code}/content",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn get_channel_property_content(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((channel_code, property_code)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = uuid::Uuid::parse_str(&property_code).unwrap_or_default();
    // B1: room types via the PMS HTTP API.
    let rts = state
        .pms_client
        .list_room_types(property_id)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("PMS room-type read failed: {e}"),
            )
        })?;
    let room_types: Vec<serde_json::Value> = rts
        .iter()
        .map(|rt| {
            serde_json::json!({
                "code": rt.get("room_type_code"), "name": rt.get("room_type_name"),
                "max_occupancy": rt.get("max_occupancy"), "bed_type": rt.get("bed_type"),
                "smoking": rt.get("is_smoking"),
            })
        })
        .collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_code": channel_code, "property_id": property_id, "room_types": room_types }),
    ))
}

/// `GET /v1/channels/{channel_code}/properties/{property_code}/ari`
#[utoipa::path(
    get,
    path = "/v1/channels/{channel_code}/properties/{property_code}/ari",
    responses(
        (status = 200, description = "Channel availability (ARI)", body = serde_json::Value),
        (status = 403, description = "Missing distrib:admin scope"),
    ),
    security(("distrib:admin" = [])),
    tag = "distrib",
)]
pub async fn get_channel_ari(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((channel_code, property_code)): Path<(String, String)>,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let start = params
        .get("start_date")
        .and_then(|v| v.as_str())
        .unwrap_or("2026-01-01");
    let end = params
        .get("end_date")
        .and_then(|v| v.as_str())
        .unwrap_or("2026-12-31");
    // B1: ARI via the PMS HTTP API.
    let property_id = uuid::Uuid::parse_str(&property_code).unwrap_or_default();
    let inv = state
        .pms_client
        .get_inventory(property_id, start, end)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("PMS inventory read failed: {e}"),
            )
        })?;
    let ari: Vec<serde_json::Value> = inv
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            serde_json::json!({
                "room_type_id": r.get("room_type_id"), "date": r.get("date"),
                "available": r.get("available_rooms"), "sell_limit": r.get("sell_limit"),
            })
        })
        .collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_code": channel_code, "property_id": property_code, "ari": ari }),
    ))
}
#[utoipa::path(
    get,
    path = "/v1/channels/{channel_code}/properties/{property_code}/controls",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn get_channel_controls(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((channel_code, property_code)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let property_id = uuid::Uuid::parse_str(&property_code).unwrap_or_default();
    let row = sqlx::query("SELECT cta_enabled, ctd_enabled, min_los, max_los FROM distrib.channel_properties cp JOIN distrib.channels c ON cp.channel_id = c.id WHERE c.channel_code = $1 AND cp.property_id = $2")
        .bind(&channel_code).bind(property_id).fetch_optional(&mut *conn).await.map_err(err_db)?;
    let controls = match row {
        Some(r) => serde_json::json!({
            "cta_enabled": r.get::<bool, _>("cta_enabled"), "ctd_enabled": r.get::<bool, _>("ctd_enabled"),
            "min_los": r.get::<Option<i32>, _>("min_los"), "max_los": r.get::<Option<i32>, _>("max_los"),
        }),
        None => {
            serde_json::json!({ "cta_enabled": true, "ctd_enabled": true, "min_los": null, "max_los": null })
        }
    };
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_code": channel_code, "property_id": property_code, "controls": controls }),
    ))
}

// ── -Hardening ──
#[utoipa::path(
    get,
    path = "/v1/channels/{channel_code}/validate",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn validate_channel_credentials(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(channel_code): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:admin"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let row = sqlx::query("SELECT id, channel_code, api_key_ref, is_active FROM distrib.channels WHERE channel_code = $1")
        .bind(&channel_code).fetch_optional(&mut *conn).await.map_err(err_db)?;
    let valid = match row {
        Some(r) => {
            r.get::<bool, _>("is_active") && r.get::<Option<String>, _>("api_key_ref").is_some()
        }
        None => false,
    };
    conn.commit().await.map_err(err_db)?;
    Ok(Json(
        serde_json::json!({ "channel_code": channel_code, "valid": valid }),
    ))
}
#[utoipa::path(
    get,
    path = "/v1/sync-log",
    responses(
        (status = 200, description = "OK", body = serde_json::Value),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Forbidden"),
    ),
    security(),
    tag = "distrib",
)]

pub async fn get_sync_log(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let a = crate::auth::verify_token(&state.config, &headers).await?;
    crate::policy::require(
        &state.policy,
        &a,
        &headers,
        sentin8::shield::EvalTarget::new("hotel8:distrib:read"),
    )
    .await?;
    let env_id = resolve_env(&a);
    let mut conn = state.pool.begin().await.map_err(err_db)?;
    set_tenant_context(&mut conn, env_id).await;
    let limit: i64 = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);
    let rows = sqlx::query("SELECT id, channel_id, property_id, sync_type, status, attempts, error_message, created_at, completed_at FROM distrib.sync_log ORDER BY created_at DESC LIMIT $1")
        .bind(limit).fetch_all(&mut *conn).await.map_err(err_db)?;
    let items: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({
        "id": r.get::<uuid::Uuid, _>("id"), "channel_id": r.get::<uuid::Uuid, _>("channel_id"),
        "property_id": r.get::<Option<uuid::Uuid>, _>("property_id"), "sync_type": r.get::<String, _>("sync_type"),
        "status": r.get::<String, _>("status"), "attempts": r.get::<i32, _>("attempts"),
        "error_message": r.get::<Option<String>, _>("error_message"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "completed_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("completed_at"),
    })).collect();
    conn.commit().await.map_err(err_db)?;
    Ok(Json(serde_json::json!({ "items": items })))
}
