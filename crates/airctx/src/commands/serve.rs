use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::server_cfg;

pub fn run(addr: Option<&str>, gate: Option<&str>, data_dir: PathBuf) -> Result<()> {
    let cfg = server_cfg::resolve(&data_dir, addr, gate)?;
    let addr: SocketAddr = format!("{}:{}", cfg.bind, cfg.port)
        .parse()
        .context("invalid listen address")?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;
    rt.block_on(airctx_server::run(airctx_server::ServeOptions {
        addr,
        data_dir,
        gate: cfg.gate,
    }))
}
