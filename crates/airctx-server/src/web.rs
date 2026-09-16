//! Gate 路径下的 Web UI 与账号 API。Cookie 会话;CLI 仍用 Bearer token。

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use airctx_core::store::{unix_now, User};

use crate::auth::ApiError;
use crate::state::AppState;

const HTML: &str = include_str!("webui.html");
const COOKIE: &str = "airctx";
const PREVIEW: usize = 64 * 1024;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(ui))
        .route("/api/status", get(status))
        .route("/api/register", post(register))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/invite", post(invite))
        .route("/api/kv", get(list_kv).delete(delete_kv))
        .route("/api/item", get(item))
}

pub async fn ui() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        HTML,
    )
        .into_response()
}

async fn status(State(app): State<Arc<AppState>>) -> Result<Json<Value>, ApiError> {
    let n = app.store.lock().await.user_count()?;
    Ok(Json(json!({ "open_register": n == 0, "users": n })))
}

#[derive(Deserialize)]
struct AuthBody {
    username: String,
    password: String,
    invite: Option<String>,
}

async fn register(State(app): State<Arc<AppState>>, Json(body): Json<AuthBody>) -> Result<Response, ApiError> {
    let now = unix_now();
    let user = {
        let mut st = app.store.lock().await;
        st.register_user(&body.username, &body.password, body.invite.as_deref(), now)?
    };
    cookie_ok(&app, user).await
}

async fn login(State(app): State<Arc<AppState>>, Json(body): Json<AuthBody>) -> Result<Response, ApiError> {
    let user = {
        let st = app.store.lock().await;
        st.verify_password(&body.username, &body.password)?
    };
    let Some(user) = user else {
        return Err(ApiError::unauthorized("invalid username or password"));
    };
    cookie_ok(&app, user).await
}

async fn cookie_ok(app: &AppState, user: User) -> Result<Response, ApiError> {
    let raw = app.store.lock().await.create_web_session(user.id, unix_now())?;
    let body = Json(json!({
        "ok": true,
        "username": user.username,
        "is_admin": user.is_admin,
        "api_token": user.api_token,
    }));
    let mut resp = body.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&set_cookie(&app.gate, &raw)).expect("cookie"),
    );
    Ok(resp)
}

async fn logout(State(app): State<Arc<AppState>>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(raw) = cookie_from(&headers) {
        let _ = app.store.lock().await.drop_web_session(&raw);
    }
    let mut resp = Json(json!({ "ok": true })).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&clear_cookie(&app.gate)).expect("cookie"),
    );
    Ok(resp)
}

async fn me(State(app): State<Arc<AppState>>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let u = require_user(&app, &headers).await?;
    Ok(Json(json!({
        "username": u.username,
        "is_admin": u.is_admin,
        "api_token": u.api_token,
    })))
}

async fn invite(State(app): State<Arc<AppState>>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let u = require_user(&app, &headers).await?;
    if !u.is_admin {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "admin only"));
    }
    let code = app.store.lock().await.create_invite(u.id, unix_now())?;
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    let url = format!("{proto}://{host}/{}/?invite={code}", app.gate);
    Ok(Json(json!({ "code": code, "url": url })))
}

#[derive(Deserialize)]
struct KvQ {
    prefix: Option<String>,
    key: Option<String>,
    limit: Option<u32>,
}

async fn list_kv(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<KvQ>,
) -> Result<Json<Value>, ApiError> {
    let _ = require_user(&app, &headers).await?;
    let prefix = q.prefix.unwrap_or_default();
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let items = app.store.lock().await.list(&prefix, limit, unix_now())?;
    Ok(Json(serde_json::to_value(items).unwrap_or(json!([]))))
}

async fn delete_kv(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<KvQ>,
) -> Result<Json<Value>, ApiError> {
    let _ = require_user(&app, &headers).await?;
    let Some(key) = q.key.filter(|s| !s.is_empty()) else {
        return Err(ApiError::bad_request("key required"));
    };
    let now = unix_now();
    let ver = app.store.lock().await.delete(&key, now)?;
    if ver.is_none() {
        return Err(ApiError::not_found("key not found"));
    }
    Ok(Json(json!({ "deleted": true, "key": key })))
}

async fn item(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<KvQ>,
) -> Result<Json<Value>, ApiError> {
    let _ = require_user(&app, &headers).await?;
    let Some(key) = q.key.filter(|s| !s.is_empty()) else {
        return Err(ApiError::bad_request("key required"));
    };
    let ent = app.store.lock().await.get(&key, unix_now())?;
    let Some(ent) = ent else {
        return Err(ApiError::not_found("key not found"));
    };
    let lossy = String::from_utf8_lossy(&ent.value);
    let text: String = lossy.chars().take(PREVIEW).collect();
    Ok(Json(json!({
        "key": key,
        "version": ent.version,
        "content_type": ent.content_type,
        "text": text,
        "truncated": lossy.chars().count() > PREVIEW,
    })))
}

async fn require_user(app: &AppState, headers: &HeaderMap) -> Result<User, ApiError> {
    let Some(raw) = cookie_from(headers) else {
        return Err(ApiError::unauthorized("not signed in"));
    };
    let u = app.store.lock().await.user_from_session(&raw, unix_now())?;
    u.ok_or_else(|| ApiError::unauthorized("not signed in"))
}

fn cookie_from(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{COOKIE}=")) {
            return Some(v.to_string());
        }
    }
    None
}

fn set_cookie(gate: &str, raw: &str) -> String {
    format!(
        "{COOKIE}={raw}; Path=/{gate}; HttpOnly; SameSite=Lax; Max-Age=2592000"
    )
}

fn clear_cookie(gate: &str) -> String {
    format!("{COOKIE}=; Path=/{gate}; HttpOnly; SameSite=Lax; Max-Age=0")
}
