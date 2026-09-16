//! daemon 运行时:同步引擎宿主 + IPC 服务 + 生命周期管理。
//!
//! - `airctx daemon run`    前台运行(调试 / 自启入口)
//! - `airctx daemon start`  后台拉起(detach)
//! - `airctx daemon stop`   IPC 优雅停止
//! - `airctx daemon install/uninstall`  注册/取消开机自启
//!
//! 日志统一写入 daemon.log,无论被谁拉起(launchd / autostart / 手动)都有据可查。

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::config::{self, Config, DaemonConfig};
use crate::http::Client;
use crate::ipc::{self, Request, Response};
use crate::sources;
use crate::sync::{self, StatusRef, Trigger, TriggerTx};

// ---------- 前台运行 ----------

struct Shared {
    status: StatusRef,
    trigger: TriggerTx,
    shutdown: tokio::sync::Notify,
    watchers: Mutex<Vec<notify::RecommendedWatcher>>,
    cfg: Mutex<Config>,
    device: Mutex<String>,
}

pub async fn run() -> Result<()> {
    if ipc::daemon_running() {
        bail!("daemon already running");
    }

    let data_dir = config::daemon_data_dir();
    std::fs::create_dir_all(&data_dir).context("failed to create data directory")?;

    // 文件日志(daemon 无终端,stdout/stderr 不可靠)
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("daemon.log"))?;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(move || log_file.try_clone().expect("failed to clone log handle"))
        .try_init();
    tracing::info!(pid = std::process::id(), "daemon starting");

    let mut cfg = match config::load().context("failed to load config")? {
        Some(c) => c,
        None => bail!("not logged in: run airctx login <url> <token> first"),
    };

    // 先占 IPC,再写 pid / 开同步,避免 bind 失败留下脏 pid 和孤儿 worker
    #[cfg(unix)]
    let sock_path = ipc::socket_path();
    #[cfg(unix)]
    let listener = {
        let _ = std::fs::remove_file(&sock_path);
        tokio::net::UnixListener::bind(&sock_path).context("failed to bind IPC socket")?
    };
    #[cfg(not(unix))]
    bail!("Windows daemon not yet supported (see roadmap)");

    write_pid();

    let device = sources::ensure_device_name(&mut cfg);
    let worker_cfg = worker_daemon_config(&cfg, &device);
    tracing::info!(
        device = %device,
        file_watches = worker_cfg.watch.len(),
        "sync sources resolved"
    );

    let client = Client::from_config_value(&cfg);
    let index_path = data_dir.join("sync-index.db");
    let (status, trigger, worker) = sync::spawn_worker(worker_cfg.clone(), client, &index_path);
    let watchers = Mutex::new(sync::start_watchers(&worker_cfg, trigger.clone()).unwrap_or_default());

    let shared = Arc::new(Shared {
        status,
        trigger,
        shutdown: tokio::sync::Notify::new(),
        watchers,
        cfg: Mutex::new(cfg),
        device: Mutex::new(device),
    });

    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = shared.shutdown.notified() => {
                    tracing::info!("shutdown requested");
                    break;
                }
                _ = shutdown_signal() => {
                    tracing::info!("termination signal received");
                    break;
                }
                accepted = listener.accept() => {
                    if let Ok((stream, _)) = accepted {
                        let shared = shared.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_conn(stream, shared).await {
                                tracing::warn!(error = %e, "IPC connection handler failed");
                            }
                        });
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    let _ = shared.trigger.send(Trigger::Shutdown);
    let _ = worker.join();
    #[cfg(unix)]
    let _ = std::fs::remove_file(&sock_path);
    remove_pid();
    tracing::info!("daemon exited");
    result
}

async fn handle_conn(stream: tokio::net::UnixStream, shared: Arc<Shared>) -> Result<()> {
    let (rx, mut tx) = stream.into_split();
    let mut reader = BufReader::new(rx);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(()); // 对端关闭
        }
        let req: Request = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response { id: 0, result: None, error: Some(format!("failed to parse request: {e}")) };
                tx.write_all((serde_json::to_string(&resp)? + "\n").as_bytes()).await?;
                continue;
            }
        };
        let resp = handle_request(&shared, &req);
        tx.write_all((serde_json::to_string(&resp)? + "\n").as_bytes()).await?;
    }
}

fn handle_request(shared: &Shared, req: &Request) -> Response {
    let result = match req.method.as_str() {
        "status" => {
            let status = shared.status.lock().unwrap().clone();
            let cfg = shared.cfg.lock().unwrap();
            let device = shared.device.lock().unwrap().clone();
            Ok(serde_json::json!({
                "running": true,
                "pid": std::process::id(),
                "server_url": cfg.server_url,
                "device": device,
                "interval_secs": cfg.daemon.interval_secs,
                "watch": sources::effective_watches(&cfg, &device),
                "sources": sources::source_states(&cfg, &device),
                "status": status,
            }))
        }
        "sync_now" => {
            let _ = shared.trigger.send(Trigger::Sync);
            Ok(serde_json::json!({ "triggered": true }))
        }
        "reload" => match config::load() {
            Ok(Some(mut cfg)) => {
                let device = sources::ensure_device_name(&mut cfg);
                let worker_cfg = worker_daemon_config(&cfg, &device);
                match sync::start_watchers(&worker_cfg, shared.trigger.clone()) {
                    Ok(new_ws) => *shared.watchers.lock().unwrap() = new_ws,
                    Err(e) => tracing::warn!(error = %e, "failed to rebuild watchers on reload"),
                }
                let _ = shared.trigger.send(Trigger::Reload(worker_cfg));
                *shared.cfg.lock().unwrap() = cfg;
                *shared.device.lock().unwrap() = device;
                Ok(serde_json::json!({ "reloaded": true }))
            }
            Ok(None) => Err("failed to reload config: not logged in".into()),
            Err(e) => Err(format!("failed to reload config: {e}")),
        },
        "shutdown" => {
            shared.shutdown.notify_one();
            Ok(serde_json::json!({ "ok": true }))
        }
        other => Err(format!("unknown method: {other}")),
    };
    match result {
        Ok(v) => Response { id: req.id, result: Some(v), error: None },
        Err(e) => Response { id: req.id, result: None, error: Some(e) },
    }
}

fn worker_daemon_config(cfg: &Config, device: &str) -> DaemonConfig {
    let mut d = cfg.daemon.clone();
    d.device_name = Some(device.to_string());
    d.watch = sources::file_watches(cfg);
    d
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

// ---------- 生命周期管理 ----------

fn pid_path() -> PathBuf {
    config::daemon_data_dir().join("daemon.pid")
}

fn write_pid() {
    let _ = std::fs::write(pid_path(), std::process::id().to_string());
}

fn remove_pid() {
    let _ = std::fs::remove_file(pid_path());
}

fn read_pid() -> Option<i32> {
    std::fs::read_to_string(pid_path()).ok()?.trim().parse().ok()
}

/// 后台拉起 daemon(detach,孤儿进程)
pub fn start() -> Result<()> {
    if ipc::daemon_running() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    Command::new(exe)
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start daemon")?;
    // 等待 IPC 就绪
    for _ in 0..20 {
        if ipc::daemon_running() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let hint = std::fs::read_to_string(config::daemon_data_dir().join("daemon.log"))
        .unwrap_or_default()
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| format!("last log: {l}"))
        .unwrap_or_else(|| "no logs; run airctx daemon run in foreground to debug".to_string());
    bail!("daemon startup timed out. {hint}")
}

pub fn stop() -> Result<()> {
    match ipc::call_daemon("shutdown", serde_json::Value::Null) {
        Ok(_) => {
            // 等进程退出
            for _ in 0..20 {
                if !ipc::daemon_running() {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            bail!("daemon stop timed out");
        }
        Err(_) => {
            // IPC 不通,fallback: 按 pid kill
            if let Some(pid) = read_pid() {
                kill_pid(pid)?;
                remove_pid();
                Ok(())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(unix)]
fn kill_pid(pid: i32) -> Result<()> {
    let out = Command::new("kill").arg(pid.to_string()).output()?;
    if !out.status.success() {
        bail!("kill {pid} failed");
    }
    Ok(())
}

#[cfg(not(unix))]
fn kill_pid(_pid: i32) -> Result<()> {
    bail!("not supported on Windows")
}

pub fn status_cli() -> Result<()> {
    match ipc::call_daemon("status", serde_json::Value::Null) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        Err(_) => {
            println!("daemon is not running");
            if let Some(pid) = read_pid() {
                println!("(stale pid file pointing to {pid}; it may have crashed)");
            }
            Ok(())
        }
    }
}

pub fn sync_now() -> Result<()> {
    ipc::call_daemon("sync_now", serde_json::Value::Null)?;
    Ok(())
}

// ---------- 开机自启 ----------

fn autostart_file() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir()?;
        Some(home.join("Library/LaunchAgents/com.airctx.daemon.plist"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = dirs::home_dir()?;
        Some(home.join(".config/autostart/airctx-daemon.desktop"))
    }
    #[cfg(not(unix))]
    None
}

#[cfg(target_os = "macos")]
fn write_autostart(exe: &std::path::Path, path: &std::path::Path) -> Result<()> {
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <plist version=\"1.0\">\n<dict>\n\
         \x20 <key>Label</key><string>com.airctx.daemon</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>{}</string>\n\
         \x20   <string>daemon</string>\n\
         \x20   <string>run</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key><true/>\n\
         \x20 <key>KeepAlive</key><false/>\n\
         </dict>\n</plist>\n",
        exe.display()
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, plist)?;
    let _ = Command::new("launchctl").args(["unload", &path.to_string_lossy()]).output();
    let out = Command::new("launchctl").args(["load", &path.to_string_lossy()]).output()?;
    if !out.status.success() {
        bail!("launchctl load failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_autostart(exe: &std::path::Path, path: &std::path::Path) -> Result<()> {
    let desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=AirCtx Daemon\n\
         Exec={} daemon run\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, desktop)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_autostart(_exe: &std::path::Path, _path: &std::path::Path) -> Result<()> {
    bail!("Windows autostart not yet supported (see roadmap)")
}

/// 自启是否已注册(None = 平台不支持)
pub fn autostart_enabled() -> Option<bool> {
    autostart_file().map(|p| p.exists())
}

pub fn install() -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    let path = autostart_file().ok_or_else(|| anyhow!("autostart is not supported on this platform"))?;
    write_autostart(&exe, &path)?;
    if !ipc::daemon_running() {
        start()?;
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let _ = stop();
    if let Some(path) = autostart_file() {
        #[cfg(target_os = "macos")]
        let _ = Command::new("launchctl").args(["unload", &path.to_string_lossy()]).output();
        if path.exists() {
            std::fs::remove_file(&path).context("failed to remove autostart config")?;
        }
    }
    Ok(())
}
