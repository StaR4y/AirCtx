use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCfg {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub gate: String,
}

fn default_bind() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    8787
}

impl ServerCfg {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("server.toml")
    }

    pub fn load(data_dir: &Path) -> Result<Option<Self>> {
        let p = Self::path(data_dir);
        let Ok(s) = std::fs::read_to_string(&p) else {
            return Ok(None);
        };
        Ok(Some(toml::from_str(&s).with_context(|| format!("invalid {}", p.display()))?))
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(Self::path(data_dir), toml::to_string_pretty(self).expect("toml"))?;
        Ok(())
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/{}", self.port, self.gate)
    }
}

pub fn generate_gate() -> String {
    const ALPH: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..12).map(|_| ALPH[rng.gen_range(0..ALPH.len())] as char).collect()
}

pub fn validate_gate(g: &str) -> Result<()> {
    if g.len() < 8 || g.len() > 64 {
        bail!("security path must be 8-64 characters");
    }
    if !g.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("security path may contain letters, digits, _ and -");
    }
    Ok(())
}

pub fn resolve(data_dir: &Path, addr_flag: Option<&str>, gate_flag: Option<&str>) -> Result<ServerCfg> {
    let existing = ServerCfg::load(data_dir)?;
    let mut bind = existing.as_ref().map(|c| c.bind.clone()).unwrap_or_else(default_bind);
    let mut port = existing.as_ref().map(|c| c.port).unwrap_or_else(default_port);
    let mut gate = existing.as_ref().map(|c| c.gate.clone()).unwrap_or_default();

    if let Ok(p) = std::env::var("AIRCTX_PORT") {
        port = p.parse().context("AIRCTX_PORT")?;
    }
    if let Ok(g) = std::env::var("AIRCTX_GATE") {
        if !g.trim().is_empty() {
            gate = g.trim().to_string();
        }
    }
    if let Some(addr) = addr_flag {
        let sock: std::net::SocketAddr = addr.parse().context("invalid --addr")?;
        bind = sock.ip().to_string();
        port = sock.port();
    }
    if let Some(g) = gate_flag {
        gate = g.to_string();
    }

    let first = existing.is_none() || gate.is_empty();
    if first && io::stdin().is_terminal() && addr_flag.is_none() && gate_flag.is_none()
        && std::env::var("AIRCTX_GATE").is_err()
    {
        eprintln!("First-time server setup");
        port = prompt_u16("Port", port)?;
        let g = prompt_line("Security path (empty = random)", "")?;
        gate = if g.is_empty() { generate_gate() } else { g };
    }
    if gate.is_empty() {
        gate = generate_gate();
    }
    validate_gate(&gate)?;

    let cfg = ServerCfg { bind, port, gate };
    cfg.save(data_dir)?;
    Ok(cfg)
}

fn prompt_line(label: &str, default: &str) -> Result<String> {
    let mut out = io::stderr();
    if default.is_empty() {
        write!(out, "{label}: ")?;
    } else {
        write!(out, "{label} [{default}]: ")?;
    }
    out.flush()?;
    let mut s = String::new();
    io::stdin().read_line(&mut s)?;
    let t = s.trim();
    Ok(if t.is_empty() { default.to_string() } else { t.to_string() })
}

fn prompt_u16(label: &str, default: u16) -> Result<u16> {
    let s = prompt_line(label, &default.to_string())?;
    s.parse().context("port")
}
