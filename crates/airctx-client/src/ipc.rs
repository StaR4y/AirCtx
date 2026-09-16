//! daemon IPC:Unix domain socket 上的行式 JSON 协议。
//! CLI/TUI 是 daemon 的控制端;Windows 命名管道在 roadmap。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(unix)]
pub fn socket_path() -> std::path::PathBuf {
    crate::config::daemon_data_dir().join("daemon.sock")
}

/// daemon 控制客户端:同步、短超时,适合 TUI 轮询
#[cfg(unix)]
pub struct DaemonHandle {
    stream: std::os::unix::net::UnixStream,
    next_id: u64,
}

#[cfg(unix)]
impl DaemonHandle {
    pub fn connect() -> std::io::Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(socket_path())?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { stream, next_id: 1 })
    }

    pub fn call(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let req = Request { id: self.next_id, method: method.to_string(), params };
        self.next_id += 1;
        let line = serde_json::to_string(&req)? + "\n";
        self.stream.write_all(line.as_bytes())?;
        let mut reader = BufReader::new(&self.stream);
        let mut buf = String::new();
        reader.read_line(&mut buf)?;
        let resp: Response = serde_json::from_str(buf.trim()).map_err(|e| anyhow!("failed to parse IPC response: {e}"))?;
        if let Some(err) = resp.error {
            return Err(anyhow!("daemon: {err}"));
        }
        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }
}

/// 一次性调用:连接 → 调用 → 返回
pub fn call_daemon(method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    #[cfg(unix)]
    {
        let mut h = DaemonHandle::connect().map_err(|e| anyhow!("daemon not running ({e})"))?;
        h.call(method, params)
    }
    #[cfg(not(unix))]
    {
        let _ = (method, params);
        Err(anyhow!("Windows daemon IPC not yet supported (see roadmap)"))
    }
}

pub fn daemon_running() -> bool {
    call_daemon("status", serde_json::Value::Null).is_ok()
}
