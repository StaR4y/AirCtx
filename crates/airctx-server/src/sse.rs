//! SSE 实时订阅:/v1/watch。
//!
//! 建立连接时可用 since_version 补发差量,再接实时广播;
//! 广播断档(lagged)时以注释提醒客户端重连对账。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, KeepAliveStream, Sse};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{BroadcastStream, UnboundedReceiverStream};
use tokio_stream::StreamExt;

use airctx_core::event::Event;
use airctx_core::store::unix_now;

use crate::auth::{ApiError, Auth};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct WatchParams {
    pub prefix: Option<String>,
    pub since_version: Option<i64>,
}

fn to_sse(ev: &Event) -> SseEvent {
    SseEvent::default()
        .event(ev.kind())
        .json_data(ev)
        .expect("Event serialization cannot fail")
}

pub async fn watch(
    State(app): State<Arc<AppState>>,
    _auth: Auth,
    Query(p): Query<WatchParams>,
) -> Result<Sse<KeepAliveStream<UnboundedReceiverStream<Result<SseEvent, Infallible>>>>, ApiError> {
    let prefix = p.prefix.unwrap_or_default();
    let (tx, rx) = mpsc::unbounded_channel();

    if let Some(since) = p.since_version {
        let now = unix_now();
        let events = app.store.lock().await.changed_since(since, &prefix, 1000, now)?;
        for ev in events {
            let _ = tx.send(Ok(to_sse(&ev)));
        }
    }

    let broadcast_rx = app.events.subscribe();
    let forward_tx = tx.clone();
    let match_prefix = prefix;
    tokio::spawn(async move {
        let mut stream = BroadcastStream::new(broadcast_rx);
        while let Some(item) = stream.next().await {
            match item {
                Ok(ev) => {
                    if ev.key().starts_with(&match_prefix) && forward_tx.send(Ok(to_sse(&ev))).is_err() {
                        break;
                    }
                }
                Err(_lagged) => {
                    let note = SseEvent::default()
                        .comment("broadcast lagged; reconnect with since_version to resync");
                    if forward_tx.send(Ok(note)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    Ok(Sse::new(UnboundedReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keep-alive")))
}
