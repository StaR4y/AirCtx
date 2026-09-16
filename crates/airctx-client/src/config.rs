use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 同步周期档位(TUI 里左右键切换)
pub const INTERVAL_CHOICES: [u64; 6] = [30, 60, 300, 900, 1800, 3600];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchEntry {
    /// 本地目录,支持 ~ 开头
    pub path: String,
    /// 远端 key 前缀
    pub prefix: String,
}

impl WatchEntry {
    pub fn resolved_path(&self) -> PathBuf {
        expand_tilde(&self.path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default)]
    pub watch: Vec<WatchEntry>,
    /// 设备名(远端按设备分类的命名空间);缺省自动生成 {hostname}-{os} 并回写
    #[serde(default)]
    pub device_name: Option<String>,
    /// 禁用的内置源 id 列表,如 ["codex"]
    #[serde(default)]
    pub disable_sources: Vec<String>,
    /// 内置源根目录覆盖,如 {"codex": "/custom/path"}
    #[serde(default)]
    pub source_overrides: HashMap<String, String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            watch: vec![],
            device_name: None,
            disable_sources: vec![],
            source_overrides: HashMap::new(),
        }
    }
}

fn default_interval() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    pub token: String,
    #[serde(default)]
    pub daemon: DaemonConfig,
}

/// 配置路径;AIRCTX_CONFIG 环境变量可覆盖(测试/多环境)
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("AIRCTX_CONFIG") {
        return PathBuf::from(p);
    }
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("airctx").join("config.toml")
}

/// daemon 数据目录(索引库/pid/socket/日志);AIRCTX_DATA_DIR 可覆盖
pub fn daemon_data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("AIRCTX_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("airctx")
}

pub fn load() -> io::Result<Option<Config>> {
    let path = config_path();
    let Ok(s) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    match toml::from_str(&s) {
        Ok(c) => Ok(Some(c)),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

pub fn normalize_url(url: &str) -> String {
    let url = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("http://{url}")
    };
    url.trim_end_matches('/').to_string()
}

/// 写入登录信息,保留已有 daemon 配置。
pub fn save_login(url: &str, token: &str) -> io::Result<Config> {
    let daemon = load().ok().flatten().map(|c| c.daemon).unwrap_or_default();
    let cfg = Config { server_url: url.to_string(), token: token.to_string(), daemon };
    save(&cfg)?;
    Ok(cfg)
}

pub fn save(cfg: &Config) -> io::Result<()> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, toml::to_string_pretty(cfg).expect("Config serialization cannot fail"))?;
    // token 属于凭据,收紧文件权限
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(p));
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

/// 路径是否存在且是目录
pub fn is_dir(p: &Path) -> bool {
    p.is_dir()
}
