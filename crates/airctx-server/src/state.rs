use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};

use airctx_core::event::Event;
use airctx_core::store::Store;

use crate::limiter::RateLimiter;

pub struct AppState {
    pub store: Mutex<Store>,
    pub events: broadcast::Sender<Arc<Event>>,
    pub limiter: RateLimiter,
    /// 安全入口路径段,Web UI 与 API 都挂在 /{gate}/ 下
    pub gate: String,
}
