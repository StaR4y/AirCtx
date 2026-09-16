//! AirCtx 服务端:axum + tokio 壳,依赖 airctx-core。

pub mod api;
pub mod auth;
pub mod limiter;
pub mod sse;
pub mod state;
pub mod web;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use tokio::sync::{broadcast, Mutex};

use airctx_core::event::Event;
use airctx_core::store::{unix_now, Store};

pub use state::AppState;
use limiter::RateLimiter;

/// 单条上下文上限。解析后的会话通常远小于此;原始附件按好用优先。
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

pub struct ServeOptions {
    pub addr: SocketAddr,
    pub data_dir: PathBuf,
    /// 安全入口:所有 UI/API 位于 /{gate}/
    pub gate: String,
}

async fn hidden() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

pub fn build_router(state: Arc<AppState>, gate: &str) -> Router {
    let v1 = Router::new()
        .route("/health", get(api::health))
        .route("/kv", get(api::list_entries))
        .route("/kv/{*key}", get(api::get_entry).put(api::put_entry).delete(api::delete_entry))
        .route("/watch", get(sse::watch));
    let gated = Router::new()
        .merge(web::routes())
        .nest("/v1", v1)
        .layer(middleware::from_fn_with_state(state.clone(), limiter_mw))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state.clone());
    Router::new()
        .route("/", get(hidden))
        // axum nest("/gate") 匹配 /gate 和 /gate/foo,但不匹配 /gate/(尾部斜杠)。
        // 浏览器地址栏常会补上 /,必须单独挂。
        .route(&format!("/{gate}/"), get(web::ui))
        .nest(&format!("/{gate}"), gated)
        .fallback(hidden)
}

async fn limiter_mw(State(app): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or_else(|| IpAddr::from([0, 0, 0, 0]));
    if !app.limiter.check_request(ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "rate limit exceeded, retry later" })),
        )
            .into_response();
    }
    next.run(req).await
}

pub async fn run(opts: ServeOptions) -> anyhow::Result<()> {
    // 0.0.0.0 能和 127.0.0.1 上已有进程并存(macOS SO_REUSEADDR)。
    // 浏览器访问 127.0.0.1 会打到那个进程,出现别人的 404 JSON。
    let loopback = SocketAddr::from(([127, 0, 0, 1], opts.addr.port()));
    if tokio::net::TcpStream::connect(loopback).await.is_ok() {
        anyhow::bail!(
            "127.0.0.1:{} is already taken (often a Java/Spring app). \
             Browsers to localhost would hit that process, not AirCtx.\n\
             pick another port: airctx serve --addr 0.0.0.0:8787 --data-dir {}",
            opts.addr.port(),
            opts.data_dir.display()
        );
    }

    let listener = match tokio::net::TcpListener::bind(opts.addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            anyhow::bail!(
                "port {} is already in use; another airctx serve may be running.\n\
                 stop it: pkill -f \"airctx serve\"\n\
                 or listen elsewhere: airctx serve --addr 0.0.0.0:8788 --data-dir ./data",
                opts.addr
            );
        }
        Err(e) => return Err(e.into()),
    };

    let db_path = opts.data_dir.join("airctx.db");
    let store = Store::open(&db_path)?;

    let (events, _) = broadcast::channel::<Arc<Event>>(1024);
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        events,
        limiter: RateLimiter::new(),
        gate: opts.gate.clone(),
    });

    // TTL 清理:低频任务,读时也有惰性过期兜底
    let purge_state = state.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        loop {
            ticker.tick().await;
            let now = unix_now();
            let purged = purge_state.store.lock().await.purge_expired(now);
            match purged {
                Ok(items) => {
                    for (key, version) in items {
                        let _ = purge_state
                            .events
                            .send(Arc::new(Event::Delete { key, version, updated_at: now }));
                    }
                }
                Err(e) => tracing::warn!(error = %e, "TTL purge failed"),
            }
        }
    });

    let app = build_router(state, &opts.gate);
    let url = format!("http://127.0.0.1:{}/{}", opts.addr.port(), opts.gate);
    tracing::info!(addr = %opts.addr, gate = %opts.gate, "AirCtx server started");
    eprintln!(
        "\n==================================================\n\
         AirCtx  {url}/\n\n\
         Open that URL in a browser.\n\
         First visitor to register becomes admin.\n\
         After that, registration closes; admin creates invite links.\n\
         Clients: airctx login {url} <token-from-webui>\n\
         ==================================================\n"
    );
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
