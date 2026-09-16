//! 内置上下文源注册表。
//!
//! Codex 不走通用目录上传:由 `codex` 模块解析 rollout jsonl 后写入
//! `devices/{device}/codex/{session_id}.json`。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::{Config, WatchEntry};

pub struct KnownSource {
    pub id: &'static str,
    pub display: &'static str,
}

pub const KNOWN_SOURCES: &[KnownSource] = &[
    KnownSource { id: "codex", display: "Codex" },
    KnownSource { id: "claude-code", display: "Claude Code" },
    KnownSource { id: "opencode", display: "OpenCode" },
];

/// 源的本地根目录(override 优先)。Codex 指向 ~/.codex(其下 sessions + archived_sessions)。
pub fn source_root(id: &str, overrides: &HashMap<String, String>) -> Option<PathBuf> {
    if let Some(p) = overrides.get(id) {
        return Some(crate::config::expand_tilde(p));
    }
    match id {
        "codex" => dirs::home_dir().map(|h| h.join(".codex")),
        "claude-code" => dirs::home_dir().map(|h| h.join(".claude").join("projects")),
        "opencode" => dirs::home_dir().map(|h| h.join(".opencode")),
        _ => None,
    }
}

pub fn source_prefix(id: &str, device: &str) -> Option<String> {
    match id {
        "codex" | "claude-code" | "opencode" => Some(format!("devices/{device}/{id}/")),
        _ => None,
    }
}

pub fn source_display(id: &str) -> String {
    KNOWN_SOURCES
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.display.to_string())
        .unwrap_or_else(|| id.to_string())
}

fn platform_tag() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "linux"
    }
    #[cfg(windows)]
    {
        "windows"
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_lowercase().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "device".into())
}

/// 稳定设备名:config 优先;否则生成 {hostname}-{os} 并回写,保证跨重启不变
pub fn ensure_device_name(cfg: &mut Config) -> String {
    if let Some(name) = cfg.daemon.device_name.as_ref() {
        if !name.trim().is_empty() {
            return name.clone();
        }
    }
    let name = format!("{}-{}", hostname(), platform_tag());
    cfg.daemon.device_name = Some(name.clone());
    let _ = crate::config::save(cfg);
    name
}

/// 仅手动 watch。内置 Codex 源由 sync 里的专用采集器处理,不能当普通文件目录扫。
pub fn file_watches(cfg: &Config) -> Vec<WatchEntry> {
    cfg.daemon.watch.clone()
}

/// 状态展示用:手动 watch + 已启用的内置源
pub fn effective_watches(cfg: &Config, device: &str) -> Vec<WatchEntry> {
    let mut ws = file_watches(cfg);
    for src in KNOWN_SOURCES {
        if cfg.daemon.disable_sources.iter().any(|d| d == src.id) {
            continue;
        }
        let Some(root) = source_root(src.id, &cfg.daemon.source_overrides) else { continue };
        if !root.is_dir() {
            continue;
        }
        if let Some(prefix) = source_prefix(src.id, device) {
            ws.push(WatchEntry { path: root.to_string_lossy().to_string(), prefix });
        }
    }
    ws
}

#[derive(Debug, serde::Serialize)]
pub struct SourceState {
    pub id: String,
    pub display: String,
    pub root: String,
    pub found: bool,
    pub enabled: bool,
    pub prefix: Option<String>,
}

pub fn source_states(cfg: &Config, device: &str) -> Vec<SourceState> {
    KNOWN_SOURCES
        .iter()
        .map(|s| {
            let root = source_root(s.id, &cfg.daemon.source_overrides).unwrap_or_default();
            let found = root.is_dir()
                || root.join("sessions").is_dir()
                || (root.file_name().and_then(|n| n.to_str()) == Some("sessions") && root.is_dir());
            let enabled = !cfg.daemon.disable_sources.iter().any(|d| d == s.id);
            let prefix = if found && enabled { source_prefix(s.id, device) } else { None };
            SourceState {
                id: s.id.to_string(),
                display: s.display.to_string(),
                root: root.to_string_lossy().to_string(),
                found,
                enabled,
                prefix,
            }
        })
        .collect()
}
