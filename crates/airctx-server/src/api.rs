//! REST handlers:/v1/kv 系列。

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;

use airctx_core::event::Event;
use airctx_core::store::{unix_now, ListItem, PutResult};

use crate::auth::{ApiError, Auth};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct PutParams {
    /// TTL,秒
    pub ttl: Option<i64>,
}

#[derive(Deserialize)]
pub struct ListParams {
    pub prefix: Option<String>,
    pub limit: Option<u32>,
}

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn put_entry(
    State(app): State<Arc<AppState>>,
    auth: Auth,
    Path(key): Path<String>,
    Query(params): Query<PutParams>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PutResult>, ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let now = unix_now();
    let mut store = app.store.lock().await;
    let res = store.put(&key, &body, &content_type, params.ttl, now)?;
    if let Err(e) = store.audit(auth.token_id, "set", &key, &auth.ip.to_string(), &auth.ua, now) {
        tracing::warn!(error = %e, "audit write failed");
    }
    drop(store);

    let _ = app.events.send(Arc::new(Event::Set {
        key: res.key.clone(),
        version: res.version,
        updated_at: res.updated_at,
    }));
    tracing::info!(token = %auth.token_name, key = %key, version = res.version, size = body.len(), "set");
    Ok(Json(res))
}

pub async fn get_entry(
    State(app): State<Arc<AppState>>,
    _auth: Auth,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let now = unix_now();
    let ent = app.store.lock().await.get(&key, now)?;
    let Some(ent) = ent else {
        return Err(ApiError::not_found(format!("key not found: {key}")));
    };

    let etag = format!("\"v{}\"", ent.version);
    // 条件请求:未变更则不传 body,省流量
    if let Some(inm) = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) {
        if inm.split(',').any(|t| t.trim() == etag) {
            let mut resp = StatusCode::NOT_MODIFIED.into_response();
            resp.headers_mut().insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
            return Ok(resp);
        }
    }

    let mut resp = Response::new(Body::from(ent.value));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&ent.content_type).map_err(|_| ApiError::internal("stored content-type is invalid"))?,
    );
    h.insert(header::ETAG, HeaderValue::from_str(&etag).unwrap());
    h.insert(HeaderName::from_static("x-ctx-version"), HeaderValue::from(ent.version));
    Ok(resp)
}

pub async fn delete_entry(
    State(app): State<Arc<AppState>>,
    auth: Auth,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let now = unix_now();
    let mut store = app.store.lock().await;
    let Some(version) = store.delete(&key, now)? else {
        return Err(ApiError::not_found(format!("key not found: {key}")));
    };
    if let Err(e) = store.audit(auth.token_id, "delete", &key, &auth.ip.to_string(), &auth.ua, now) {
        tracing::warn!(error = %e, "audit write failed");
    }
    drop(store);

    let _ = app.events.send(Arc::new(Event::Delete { key: key.clone(), version, updated_at: now }));
    tracing::info!(token = %auth.token_name, key = %key, version, "delete");
    Ok(Json(json!({ "deleted": true, "key": key, "version": version })))
}

pub async fn list_entries(
    State(app): State<Arc<AppState>>,
    _auth: Auth,
    Query(p): Query<ListParams>,
) -> Result<Json<Vec<ListItem>>, ApiError> {
    let prefix = p.prefix.unwrap_or_default();
    let limit = p.limit.unwrap_or(5000).clamp(1, 50_000);
    let now = unix_now();
    let items = app.store.lock().await.list(&prefix, limit, now)?;
    Ok(Json(items))
}
