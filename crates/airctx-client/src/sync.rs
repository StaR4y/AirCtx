//! 同步引擎:监控目录 → 变化检测(mtime/size 索引)→ 上传。
//! 跑在独立 OS 线程,避免同步 IO 阻塞 tokio;notify 事件经 debounce 后唤醒。

use anyhow::Result;
use notify::Watcher;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use airctx_core::store::unix_now;

use crate::codex;
use crate::config::DaemonConfig;
use crate::http::Client;

/// 单文件大小上限(与服务端一致)
const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncStatus {
    pub started_at: i64,
    pub last_scan_at: Option<i64>,
    pub scanned_files: usize,
    pub uploads_ok: u64,
    pub uploads_failed: u64,
    /// 超过单值上限被跳过的文件数(超大 Codex 会话等)
    pub skipped_large: u64,
    pub last_error: Option<String>,
    pub syncing: bool,
    pub interval_secs: u64,
}

impl SyncStatus {
    fn new(interval_secs: u64) -> Self {
        Self {
            started_at: unix_now(),
            last_scan_at: None,
            scanned_files: 0,
            uploads_ok: 0,
            uploads_failed: 0,
            skipped_large: 0,
            last_error: None,
            syncing: false,
            interval_secs,
        }
    }
}

/// 唤醒 worker 的信号
pub enum Trigger {
    /// 立即扫描(手动/文件变化)
    Sync,
    /// 重载配置后扫描
    Reload(DaemonConfig),
    Shutdown,
}

pub type StatusRef = Arc<Mutex<SyncStatus>>;
pub type TriggerTx = Sender<Trigger>;

/// 启动同步 worker 线程,返回状态引用与触发通道
pub fn spawn_worker(cfg: DaemonConfig, client: Client, index_path: &Path) -> (StatusRef, TriggerTx, std::thread::JoinHandle<()>) {
    let status = Arc::new(Mutex::new(SyncStatus::new(cfg.interval_secs)));
    let (tx, rx) = std::sync::mpsc::channel::<Trigger>();
    let st = status.clone();
    let index_path = index_path.to_path_buf();
    let handle = std::thread::spawn(move || {
        let mut cfg = cfg;
        let conn = match Connection::open(&index_path) {
            Ok(c) => c,
            Err(e) => {
                let mut s = st.lock().unwrap();
                s.last_error = Some(format!("failed to open index db: {e}"));
                return;
            }
        };
        if let Err(e) = init_index(&conn) {
            let mut s = st.lock().unwrap();
            s.last_error = Some(format!("failed to init index: {e}"));
            return;
        }
        // 启动即同步一次(授权后立即把存量内容传上去)
        run_sync(&cfg, &client, &conn, &st);
        loop {
            let interval = Duration::from_secs(cfg.interval_secs.max(1));
            match rx.recv_timeout(interval) {
                Ok(Trigger::Shutdown) => break,
                Ok(Trigger::Reload(new_cfg)) => {
                    cfg = new_cfg;
                    let mut s = st.lock().unwrap();
                    s.interval_secs = cfg.interval_secs;
                    drop(s);
                    run_sync(&cfg, &client, &conn, &st);
                }
                Ok(Trigger::Sync) | Err(RecvTimeoutError::Timeout) => {
                    // 防抖:同批文件事件积压在通道里,drain 后一次扫描处理
                    while let Ok(Trigger::Sync) = rx.try_recv() {}
                    run_sync(&cfg, &client, &conn, &st);
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    (status, tx, handle)
}

fn init_index(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS sync_index (
            path TEXT NOT NULL,
            prefix TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            size INTEGER NOT NULL,
            version INTEGER NOT NULL,
            uploaded_at INTEGER NOT NULL,
            PRIMARY KEY (path, prefix)
         );",
    )
}

fn run_sync(cfg: &DaemonConfig, client: &Client, conn: &Connection, status: &StatusRef) {
    {
        let mut s = status.lock().unwrap();
        if s.syncing {
            return; // 上一次还没跑完,跳过本轮
        }
        s.syncing = true;
        s.skipped_large = 0; // counted per round
        s.uploads_failed = 0;
    }
    let result = sync_once(cfg, client, conn, status);
    let mut s = status.lock().unwrap();
    s.syncing = false;
    s.last_scan_at = Some(unix_now());
    match result {
        Err(e) => {
            s.last_error = Some(e.to_string());
            tracing::warn!(error = %e, "sync failed");
        }
        // 整体成功且无单文件失败才清空错误
        Ok(0) => s.last_error = None,
        Ok(_) => {}
    }
}

fn sync_once(cfg: &DaemonConfig, client: &Client, conn: &Connection, status: &StatusRef) -> Result<usize> {
    let mut scanned = 0usize;
    let mut failed = 0usize;
    for watch in &cfg.watch {
        let root = watch.resolved_path();
        if !root.is_dir() {
            tracing::warn!(path = %watch.path, "watch dir missing, skipped");
            continue;
        }
        let prefix = normalize_prefix(&watch.prefix);
        for entry in walkdir::WalkDir::new(&root).follow_links(false) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(&root) else { continue };
            if should_skip(rel) {
                continue;
            }
            scanned += 1;
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > MAX_FILE_SIZE {
                tracing::warn!(path = ?entry.path(), size = meta.len(), "exceeds size limit, skipped");
                status.lock().unwrap().skipped_large += 1;
                continue;
            }
            let mtime = mtime_secs(&meta);
            let size = meta.len() as i64;
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let key = format!("{prefix}{rel_str}");

            let known: Option<(i64, i64)> = conn
                .query_row(
                    "SELECT mtime, size FROM sync_index WHERE path = ?1 AND prefix = ?2",
                    params![rel_str, prefix],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;

            if let Some((m, s)) = known {
                if m == mtime && s == size {
                    continue; // 未变化
                }
            }

            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            match client.put(&key, &bytes, None, content_type_for(&rel_str)) {
                Ok(res) => {
                    conn.execute(
                        "INSERT INTO sync_index(path, prefix, mtime, size, version, uploaded_at)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(path, prefix) DO UPDATE SET
                           mtime=excluded.mtime, size=excluded.size,
                           version=excluded.version, uploaded_at=excluded.uploaded_at",
                        params![rel_str, prefix, mtime, size, res.version, unix_now()],
                    )?;
                    let mut s = status.lock().unwrap();
                    s.uploads_ok += 1;
                }
                Err(e) => {
                    tracing::warn!(key = %key, error = %e, "upload failed");
                    let mut s = status.lock().unwrap();
                    s.uploads_failed += 1;
                    s.last_error = Some(e.to_string());
                    failed += 1;
                }
            }
        }
    }
    {
        let mut s = status.lock().unwrap();
        s.scanned_files = scanned;
    }
    failed += crate::codex::sync_sessions(cfg, client, conn, status)?;
    Ok(failed)
}

/// 为手动 watch 目录 + Codex sessions 目录安装文件 watcher;事件进入通道,由 worker 侧防抖合并
pub fn start_watchers(cfg: &DaemonConfig, trigger: TriggerTx) -> Result<Vec<notify::RecommendedWatcher>> {
    let mut roots: Vec<std::path::PathBuf> = cfg
        .watch
        .iter()
        .map(|w| w.resolved_path())
        .filter(|p| p.is_dir())
        .collect();
    for p in codex::watch_dirs(cfg) {
        if !roots.iter().any(|r| r == &p) {
            roots.push(p);
        }
    }
    let mut watchers = Vec::new();
    for root in roots {
        let tx = trigger.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if res.is_ok() {
                let _ = tx.send(Trigger::Sync);
            }
        })?;
        watcher.watch(&root, notify::RecursiveMode::Recursive)?;
        watchers.push(watcher);
    }
    Ok(watchers)
}

fn normalize_prefix(prefix: &str) -> String {
    let p = prefix.trim_matches('/');
    if p.is_empty() {
        String::new()
    } else {
        format!("{p}/")
    }
}

fn mtime_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 跳过隐藏文件与临时文件
fn should_skip(rel: &Path) -> bool {
    rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.starts_with('.') || s.ends_with('~')
    }) || {
        let name = rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        matches!(
            rel.extension().and_then(|e| e.to_str()).unwrap_or(""),
            "swp" | "tmp" | "bak" | "part" | "crdownload"
        ) || name.starts_with('#')
    }
}

pub fn content_type_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" => "text/plain",
        "json" => "application/json",
        "jsonl" | "ndjson" => "application/x-ndjson",
        "yaml" | "yml" => "text/yaml",
        "toml" => "text/toml",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "ts" => "text/typescript",
        "rs" | "py" | "go" | "java" | "c" | "h" | "cpp" | "hpp" | "sh" | "rb" | "php" | "sql" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}
