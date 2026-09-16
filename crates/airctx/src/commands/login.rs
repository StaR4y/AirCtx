use anyhow::{bail, Context, Result};

use airctx_client::config;
use airctx_client::http::Client;

pub fn run(url: &str, token: &str) -> Result<()> {
    if token.trim().is_empty() {
        bail!("token is empty: open the security URL in a browser, sign in, and copy the CLI token");
    }
    let url = config::normalize_url(url);

    let client = Client::new(url.clone(), token.to_string());
    client.list(None, 1).context("login failed: server unreachable or invalid token")?;

    config::save_login(&url, token)?;
    println!("logged in to {url}");
    println!("config saved to {}", config::config_path().display());
    println!("all client traffic uses this security URL");
    Ok(())
}
