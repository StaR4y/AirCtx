use anyhow::{bail, Context, Result};
use std::io::{BufRead, IsTerminal, Read, Write};

use airctx_client::http::Client;

pub fn set(key: &str, value: Option<String>, ttl: Option<&str>, content_type: &str) -> Result<()> {
    let data: Vec<u8> = match value.as_deref() {
        Some("-") => read_stdin()?,
        Some(v) => v.as_bytes().to_vec(),
        // 管道/重定向时无值参数也读 stdin
        None if !std::io::stdin().is_terminal() => read_stdin()?,
        None => bail!("usage: airctx set <key> <value> | airctx set <key> - < file | echo xxx | airctx set <key> -"),
    };
    let ttl = ttl.map(parse_ttl).transpose().context("failed to parse --ttl")?;
    let client = Client::from_config()?;
    let res = client.put(key, &data, ttl, content_type)?;
    println!("{} -> v{}", res.key, res.version);
    Ok(())
}

fn read_stdin() -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf).context("failed to read stdin")?;
    Ok(buf)
}

/// 接受秒数或 humantime 格式(90s / 10m / 2h)
fn parse_ttl(s: &str) -> Result<u64> {
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    Ok(humantime::parse_duration(s)?.as_secs())
}

pub fn get(key: &str, meta: bool) -> Result<()> {
    let client = Client::from_config()?;
    let res = client.get(key).context("fetch failed")?;
    if meta {
        eprintln!("version: v{}\ncontent-type: {}", res.version, res.content_type);
    }
    std::io::stdout().lock().write_all(&res.bytes)?;
    Ok(())
}

pub fn ls(prefix: Option<&str>, json: bool, limit: u32) -> Result<()> {
    let client = Client::from_config()?;
    let items = client.list(prefix, limit)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("(empty)");
        return Ok(());
    }
    for it in &items {
        let exp = it
            .expires_at
            .map(|t| format!("  expires {}", fmt_ts(t)))
            .unwrap_or_default();
        println!(
            "{:<48} {:>7} {:>9}  {}{}",
            it.key,
            format!("v{}", it.version),
            human_size(it.size),
            fmt_ts(it.updated_at),
            exp
        );
    }
    eprintln!("{} items", items.len());
    Ok(())
}

pub fn rm(key: &str) -> Result<()> {
    let client = Client::from_config()?;
    let version = client.delete(key).context("delete failed")?;
    println!("deleted {key} (v{version})");
    Ok(())
}

pub fn watch(prefix: Option<&str>, since: Option<i64>) -> Result<()> {
    let client = Client::from_config()?;
    let stream = client.watch_stream(prefix, since)?;
    println!("connected, watching prefix {:?} (Ctrl-C to exit)", prefix.unwrap_or(""));
    let mut cur_event: Option<String> = None;
    let mut cur_data: Option<String> = None;
    for line in stream.lines() {
        let line = line.context("SSE connection closed")?;
        if let Some(rest) = line.strip_prefix("event: ") {
            cur_event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            cur_data = Some(rest.to_string());
        } else if line.is_empty() {
            if let Some(d) = cur_data.take() {
                print_event(&d);
            }
            cur_event = None;
        }
        let _ = cur_event;
    }
    Ok(())
}

fn print_event(data: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let key = v.get("key").and_then(|t| t.as_str()).unwrap_or("?");
    let version = v.get("version").and_then(|t| t.as_i64()).unwrap_or(0);
    let ts = v.get("updated_at").and_then(|t| t.as_i64()).unwrap_or(0);
    match kind {
        "set" => println!("+ {key}  v{version}  {}", fmt_ts(ts)),
        "delete" => println!("- {key}  v{version}  {}", fmt_ts(ts)),
        _ => {}
    }
}

fn fmt_ts(ts: i64) -> String {
    humantime::format_rfc3339_seconds(std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts.max(0) as u64))
        .to_string()
}

fn human_size(n: usize) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if f < K {
        format!("{n}B")
    } else if f < K * K {
        format!("{:.1}K", f / K)
    } else {
        format!("{:.1}M", f / (K * K))
    }
}
