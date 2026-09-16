//! Bearer token 认证 extractor 与统一 API 错误。

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRef, FromRequestParts};
use axum::http::{header, request::Parts, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

use crate::state::AppState;

pub struct Auth {
    pub token_id: i64,
    pub token_name: String,
    pub ip: IpAddr,
    pub ua: String,
}

impl<S> FromRequestParts<S> for Auth
where
    S: Send + Sync,
    Arc<AppState>: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = Arc::<AppState>::from_ref(state);

        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
            .unwrap_or_else(|| IpAddr::from([0, 0, 0, 0]));
        let ua = parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_owned)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ApiError::unauthorized("missing Bearer token"))?;

        // 单独作用域持有锁,保证 guard 在进入分支前释放
        let info = {
            let mut store = app.store.lock().await;
            store.verify_token(&token)
        };
        match info {
            Some(info) => Ok(Auth { token_id: info.id, token_name: info.name, ip, ua }),
            None => {
                app.limiter.report_auth_failure(ip);
                tracing::warn!(%ip, "auth failed: invalid token");
                Err(ApiError::unauthorized("invalid token"))
            }
        }
    }
}

pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl From<airctx_core::error::Error> for ApiError {
    fn from(e: airctx_core::error::Error) -> Self {
        match e {
            airctx_core::error::Error::InvalidRequest(m) => ApiError::bad_request(m),
            other => {
                tracing::error!(error = %other, "store error");
                ApiError::internal("internal error")
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}
