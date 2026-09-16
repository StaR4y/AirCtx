use serde::{Deserialize, Serialize};

/// 一条上下文变更事件,经 SSE 推送给 watch 订阅者。
/// version 全局单调递增,客户端重连时用它做增量对账。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Set { key: String, version: i64, updated_at: i64 },
    Delete { key: String, version: i64, updated_at: i64 },
}

impl Event {
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Set { .. } => "set",
            Event::Delete { .. } => "delete",
        }
    }

    pub fn key(&self) -> &str {
        match self {
            Event::Set { key, .. } | Event::Delete { key, .. } => key,
        }
    }
}
