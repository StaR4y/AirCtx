use anyhow::{bail, Result};
use std::path::Path;

use airctx_client::config;
use airctx_client::http::Client;

use super::server_cfg::ServerCfg;

pub fn run(url: Option<&str>, data_dir: Option<&Path>) -> Result<()> {
    let base = if let Some(u) = url {
        u.trim_end_matches('/').to_string()
    } else if let Some(dir) = data_dir {
        let Some(cfg) = ServerCfg::load(dir)? else {
            bail!("no server.toml in {}", dir.display());
        };
        cfg.base_url()
    } else {
        let Some(cfg) = config::load()? else {
            bail!("no URL given and not logged in; usage: airctx ping <url-with-gate>");
        };
        cfg.server_url
    };
    Client::ping(&base)?;
    println!("ok {base}");
    Ok(())
}
