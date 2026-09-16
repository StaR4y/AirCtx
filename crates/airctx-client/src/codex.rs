//! Codex 上下文采集:发现 rollout jsonl → 解析完整会话 → 上传 JSON。
//!
//! 不再把原始 jsonl(含加密 thinking、token 计数、16MB+ 文件)当上下文;
//! 超大会话在上传前只收缩 tool/reasoning 文本,用户与助手消息保留。

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::DaemonConfig;
use crate::http::Client;
use crate::sessions::{self, SessionView};
use crate::sync::StatusRef;
use airctx_core::store::unix_now;

const INDEX_PREFIX: &str = "__codex__";

pub fn enabled(cfg: &DaemonConfig) -> bool {
    !cfg.disable_sources.iter().any(|d| d == "codex")
}

/// notify 监控的目录:sessions + archived_sessions
pub fn watch_dirs(cfg: &DaemonConfig) -> Vec<PathBuf> {
    rollout_roots(cfg).into_iter().filter(|p| p.is_dir()).collect()
}

fn codex_home(cfg: &DaemonConfig) -> PathBuf {
    if let Some(p) = cfg.source_overrides.get("codex") {
        let p = crate::config::expand_tilde(p);
        if p.file_name().and_then(|n| n.to_str()) == Some("sessions") {
            return p.parent().map(Path::to_path_buf).unwrap_or(p);
        }
        return p;
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
}

fn rollout_roots(cfg: &DaemonConfig) -> Vec<PathBuf> {
    let home = codex_home(cfg);
    vec![home.join("sessions"), home.join("archived_sessions")]
}

fn discover_rollouts(cfg: &DaemonConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in rollout_roots(cfg) {
        if !root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&root).follow_links(false) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                out.push(entry.into_path());
            }
        }
    }
    out
}

fn load_titles(home: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let path = home.join("session_index.jsonl");
    let Ok(file) = File::open(path) else { return map };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(id) = v.get("id").and_then(|x| x.as_str()) else { continue };
        if let Some(name) = v.get("thread_name").and_then(|x| x.as_str()) {
            if !name.is_empty() {
                map.insert(id.to_string(), name.to_string());
            }
        }
    }
    map
}

/// 从文件名拆 session / fork id:`rollout-{YYYY-MM-DD}T{HH-MM-SS}-{id}[_{fork}].jsonl`
pub fn ids_from_filename(name: &str) -> (String, String) {
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    let rest = stem.strip_prefix("rollout-").unwrap_or(stem);
    // 丢掉 `2026-09-17T00-39-58-` 前缀,剩下 uuid 或 uuid_uuid
    let ids = if let Some(pos) = rest.find('T') {
        let after_t = &rest[pos + 1..];
        let mut segs = after_t.split('-');
        let _h = segs.next();
        let _m = segs.next();
        let _s = segs.next();
        segs.collect::<Vec<_>>().join("-")
    } else {
        rest.to_string()
    };
    match ids.split_once('_') {
        Some((a, b)) if !a.is_empty() && !b.is_empty() => (a.to_string(), b.to_string()),
        _ => (ids, String::new()),
    }
}

pub fn remote_key(device: &str, session_id: &str, fork_id: &str) -> String {
    if fork_id.is_empty() {
        format!("devices/{device}/codex/{session_id}.json")
    } else {
        format!("devices/{device}/codex/{session_id}/{fork_id}.json")
    }
}

fn extract_file(path: &Path) -> SessionView {
    let Ok(file) = File::open(path) else {
        return SessionView::default();
    };
    let mut view = sessions::parse_jsonl_reader(BufReader::new(file));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let (file_id, fork) = ids_from_filename(name);
    if view.session_id.is_empty() {
        view.session_id = file_id;
    }
    if view.fork_id.is_empty() {
        view.fork_id = fork;
    }
    view
}

fn mtime_size(path: &Path) -> Option<(i64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((mtime, meta.len() as i64))
}

/// 正在写入的会话:mtime 太新则本轮跳过,等下一轮对账拿完整尾部
fn too_fresh(mtime: i64) -> bool {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    now.saturating_sub(mtime) < 2
}

pub fn sync_sessions(cfg: &DaemonConfig, client: &Client, conn: &Connection, status: &StatusRef) -> Result<usize> {
    if !enabled(cfg) {
        return Ok(0);
    }
    let device = cfg
        .device_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("device");
    let titles = load_titles(&codex_home(cfg));
    let files = discover_rollouts(cfg);
    let mut failed = 0usize;
    let mut scanned = 0usize;
    let index_prefix = format!("{INDEX_PREFIX}:{device}");

    for path in files {
        scanned += 1;
        let Some((mtime, size)) = mtime_size(&path) else { continue };
        if too_fresh(mtime) {
            continue;
        }
        let rel = path.to_string_lossy().replace('\\', "/");
        let known: Option<(i64, i64)> = conn
            .query_row(
                "SELECT mtime, size FROM sync_index WHERE path = ?1 AND prefix = ?2",
                params![rel, index_prefix],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((m, s)) = known {
            if m == mtime && s == size {
                continue;
            }
        }

        let mut view = extract_file(&path);
        if view.session_id.is_empty() {
            tracing::warn!(path = %path.display(), "codex rollout missing session_id, skipped");
            continue;
        }
        if view.title.is_empty() {
            if let Some(t) = titles.get(&view.session_id) {
                view.title = t.clone();
            } else {
                view.title = view.session_id.clone();
            }
        }
        let key = remote_key(device, &view.session_id, &view.fork_id);
        let (bytes, fitted) = sessions::encode_for_upload(view);
        if fitted.truncated {
            tracing::info!(key = %key, bytes = bytes.len(), "codex session fitted to upload limit");
        }
        match client.put(&key, &bytes, None, "application/json") {
            Ok(res) => {
                conn.execute(
                    "INSERT INTO sync_index(path, prefix, mtime, size, version, uploaded_at)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(path, prefix) DO UPDATE SET
                       mtime=excluded.mtime, size=excluded.size,
                       version=excluded.version, uploaded_at=excluded.uploaded_at",
                    params![rel, index_prefix, mtime, size, res.version, unix_now()],
                )?;
                let mut s = status.lock().unwrap();
                s.uploads_ok += 1;
            }
            Err(e) => {
                tracing::warn!(key = %key, error = %e, "codex session upload failed");
                let mut s = status.lock().unwrap();
                s.uploads_failed += 1;
                s.last_error = Some(e.to_string());
                failed += 1;
            }
        }
    }

    let mut s = status.lock().unwrap();
    s.scanned_files = s.scanned_files.saturating_add(scanned);
    Ok(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_ids_plain_and_fork() {
        let (id, fork) = ids_from_filename(
            "rollout-2026-09-17T00-39-58-01a09ebe-3b17-78d2-a9bd-96d9106d0f29.jsonl",
        );
        assert_eq!(id, "01a09ebe-3b17-78d2-a9bd-96d9106d0f29");
        assert!(fork.is_empty());

        let (id, fork) = ids_from_filename(
            "rollout-2026-09-14T15-08-52-01a09ebe-3b17-78d2-a9bd-96d9106d0f29_01a09ebf-4ca1-7f80-a228-6d1d720c0f72.jsonl",
        );
        assert_eq!(id, "01a09ebe-3b17-78d2-a9bd-96d9106d0f29");
        assert_eq!(fork, "01a09ebf-4ca1-7f80-a228-6d1d720c0f72");
    }

    #[test]
    fn remote_key_shape() {
        assert_eq!(remote_key("starry-macos", "abc", ""), "devices/starry-macos/codex/abc.json");
        assert_eq!(
            remote_key("starry-macos", "abc", "fork"),
            "devices/starry-macos/codex/abc/fork.json"
        );
    }
}
