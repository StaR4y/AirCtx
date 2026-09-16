//! HTTP 客户端(ureq,同步)。
//! 普通操作走带超时的 agent;watch 走无读超时的 agent 以保持 SSE 长连接。

use anyhow::{anyhow, Result};
use std::io::Read;
use std::time::Duration;

use airctx_core::store::{ListItem, PutResult};

use crate::config::Config;

/// 单响应体防御上限
const MAX_RESPONSE: u64 = 64 * 1024 * 1024;

pub struct Client {
    base: String,
    token: String,
    agent: ureq::Agent,
    stream_agent: ureq::Agent,
}

pub struct GetResult {
    pub bytes: Vec<u8>,
    pub version: i64,
    pub content_type: String,
}

/// key 做百分号编码,'/' 保留为路径分隔
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Client {
    pub fn from_config() -> Result<Self> {
        let cfg: Config = crate::config::load()?
            .ok_or_else(|| anyhow!("not logged in: run airctx login <url> <token> first"))?;
        Ok(Self::new(cfg.server_url, cfg.token))
    }

    pub fn from_config_value(cfg: &Config) -> Self {
        Self::new(cfg.server_url.clone(), cfg.token.clone())
    }

    pub fn new(base: String, token: String) -> Self {
        let base = base.trim_end_matches('/').to_string();
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(60))
            .timeout_write(Duration::from_secs(60))
            .build();
        let stream_agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .build();
        Self { base, token, agent, stream_agent }
    }

    fn req(&self, method: &str, path: &str) -> ureq::Request {
        self.agent
            .request(method, &format!("{}{}", self.base, path))
            .set("Authorization", &format!("Bearer {}", self.token))
    }

    fn handle_err(e: ureq::Error) -> anyhow::Error {
        match e {
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                    .unwrap_or(body);
                anyhow!("HTTP {code}: {msg}")
            }
            other => anyhow!(other),
        }
    }

    /// 探活:GET {base}/v1/health(公开接口,无需 token),供 HEALTHCHECK/脚本使用
    pub fn ping(base: &str) -> Result<()> {
        let resp = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .build()
            .get(&format!("{}/v1/health", base.trim_end_matches('/')))
            .call()
            .map_err(Self::handle_err)?;
        if resp.status() == 200 {
            Ok(())
        } else {
            Err(anyhow!("unexpected status {}", resp.status()))
        }
    }

    pub fn put(&self, key: &str, value: &[u8], ttl_secs: Option<u64>, content_type: &str) -> Result<PutResult> {
        let req = self
            .req("PUT", &format!("/v1/kv/{}", enc(key)))
            .set("Content-Type", content_type);
        let req = match ttl_secs {
            Some(t) => req.query("ttl", &t.to_string()),
            None => req,
        };
        let resp = req.send_bytes(value).map_err(Self::handle_err)?;
        Ok(serde_json::from_reader(resp.into_reader())?)
    }

    pub fn get(&self, key: &str) -> Result<GetResult> {
        let resp = self.req("GET", &format!("/v1/kv/{}", enc(key))).call().map_err(Self::handle_err)?;
        let version = resp
            .header("x-ctx-version")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        let content_type = resp.header("content-type").unwrap_or("application/octet-stream").to_string();
        let mut bytes = Vec::new();
        resp.into_reader().take(MAX_RESPONSE).read_to_end(&mut bytes)?;
        Ok(GetResult { bytes, version, content_type })
    }

    pub fn list(&self, prefix: Option<&str>, limit: u32) -> Result<Vec<ListItem>> {
        let req = self.req("GET", "/v1/kv");
        let req = match prefix {
            Some(p) => req.query("prefix", p),
            None => req,
        };
        let resp = req.query("limit", &limit.to_string()).call().map_err(Self::handle_err)?;
        Ok(serde_json::from_reader(resp.into_reader())?)
    }

    pub fn delete(&self, key: &str) -> Result<i64> {
        let resp = self
            .req("DELETE", &format!("/v1/kv/{}", enc(key)))
            .call()
            .map_err(Self::handle_err)?;
        let v: serde_json::Value = serde_json::from_reader(resp.into_reader())?;
        Ok(v.get("version").and_then(|x| x.as_i64()).unwrap_or(0))
    }

    /// 返回 SSE 字节流,由调用方逐行解析
    pub fn watch_stream(&self, prefix: Option<&str>, since: Option<i64>) -> Result<impl std::io::BufRead> {
        let req = self
            .stream_agent
            .get(&format!("{}/v1/watch", self.base))
            .set("Authorization", &format!("Bearer {}", self.token));
        let req = match prefix {
            Some(p) => req.query("prefix", p),
            None => req,
        };
        let req = match since {
            Some(s) => req.query("since_version", &s.to_string()),
            None => req,
        };
        let resp = req.call().map_err(Self::handle_err)?;
        Ok(std::io::BufReader::new(resp.into_reader()))
    }
}
