use airctx_client::sessions::{EvKind, SessionView};
use airctx_client::sources;
use airctx_core::store::{unix_now, ListItem as CtxItem};
use unicode_width::UnicodeWidthStr;

/// 列表主标题 + 副标题
pub fn key_info(key: &str) -> (String, String) {
    if let Some(pos) = key.find("/sessions/") {
        let rest = &key[pos + 10..];
        let seg: Vec<&str> = rest.splitn(4, '/').collect();
        if seg.len() >= 4 {
            let date = format!("{}/{}/{}", seg[0], seg[1], seg[2]);
            let sub = seg[3]
                .strip_prefix("rollout-")
                .and_then(|r| r.split('T').nth(1))
                .map(|tt| {
                    let tp: Vec<&str> = tt.split('-').collect();
                    if tp.len() >= 2 {
                        format!("{}:{}", tp[0], tp[1])
                    } else {
                        String::new()
                    }
                })
                .unwrap_or_default();
            return (date, sub);
        }
    }
    if let Some(rest) = key.split("/codex/").nth(1) {
        if rest.ends_with(".json") {
            let stem = rest.trim_end_matches(".json").trim_end_matches('/');
            return match stem.rsplit_once('/') {
                Some((parent, fork)) => (short_id(fork), parent.to_string()),
                None => (short_id(stem), "codex".to_string()),
            };
        }
    }
    let parts: Vec<&str> = key.split('/').collect();
    let file = parts.last().copied().unwrap_or(key);
    let dir = if parts.len() > 1 { parts[parts.len() - 2] } else { "" };
    (file.to_string(), dir.to_string())
}

fn short_id(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..8])
    } else {
        id.to_string()
    }
}

#[derive(Clone)]
pub struct DeviceRow {
    pub name: String,
    pub count: usize,
    pub last_updated: Option<i64>,
    pub is_local: bool,
}

#[derive(Clone)]
pub struct ClientRow {
    pub id: String,
    pub display: String,
    pub count: usize,
    pub last_updated: Option<i64>,
}

pub fn device_and_client(key: &str) -> Option<(&str, &str)> {
    let mut p = key.split('/');
    if p.next() != Some("devices") {
        return None;
    }
    Some((p.next()?, p.next()?))
}

pub fn devices_from_items(items: &[CtxItem], local: Option<&str>) -> Vec<DeviceRow> {
    let mut rows: Vec<DeviceRow> = Vec::new();
    let bump = |rows: &mut Vec<DeviceRow>, name: &str, ts: i64| {
        if let Some(r) = rows.iter_mut().find(|r| r.name == name) {
            r.count += 1;
            r.last_updated = Some(r.last_updated.map(|t| t.max(ts)).unwrap_or(ts));
        } else {
            rows.push(DeviceRow {
                name: name.to_string(),
                count: 1,
                last_updated: Some(ts),
                is_local: local == Some(name),
            });
        }
    };
    for it in items {
        if let Some((dev, _)) = device_and_client(&it.key) {
            bump(&mut rows, dev, it.updated_at);
        }
    }
    if let Some(local) = local {
        if !local.is_empty() && !rows.iter().any(|r| r.name == local) {
            rows.insert(
                0,
                DeviceRow { name: local.to_string(), count: 0, last_updated: None, is_local: true },
            );
        }
    }
    rows.sort_by(|a, b| match (b.is_local, a.is_local) {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => b.last_updated.cmp(&a.last_updated),
    });
    if let Some(i) = rows.iter().position(|r| r.is_local) {
        let local = rows.remove(i);
        rows.insert(0, local);
    }
    rows
}

pub fn clients_for_device(items: &[CtxItem], device: &str) -> Vec<ClientRow> {
    let mut rows: Vec<ClientRow> = sources::KNOWN_SOURCES
        .iter()
        .map(|s| ClientRow {
            id: s.id.to_string(),
            display: s.display.to_string(),
            count: 0,
            last_updated: None,
        })
        .collect();
    for it in items {
        let Some((dev, client)) = device_and_client(&it.key) else { continue };
        if dev != device {
            continue;
        }
        if let Some(r) = rows.iter_mut().find(|r| r.id == client) {
            r.count += 1;
            r.last_updated = Some(r.last_updated.map(|t| t.max(it.updated_at)).unwrap_or(it.updated_at));
        } else {
            rows.push(ClientRow {
                id: client.to_string(),
                display: sources::source_display(client),
                count: 1,
                last_updated: Some(it.updated_at),
            });
        }
    }
    rows
}

pub fn context_prefix(device: Option<&str>, client: Option<&str>) -> String {
    match (device, client) {
        (Some(d), Some(c)) => format!("devices/{d}/{c}/"),
        (Some(d), None) => format!("devices/{d}/"),
        _ => String::new(),
    }
}

pub fn matches_filter(item: &CtxItem, prefix: &str, filter: &str) -> bool {
    if !item.key.starts_with(prefix) {
        return false;
    }
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_ascii_lowercase();
    if item.key.to_ascii_lowercase().contains(&f) {
        return true;
    }
    let (title, sub) = key_info(&item.key);
    title.to_ascii_lowercase().contains(&f) || sub.to_ascii_lowercase().contains(&f)
}

pub struct DetailView {
    pub key: String,
    pub title: String,
    pub meta: String,
    pub body: Vec<LineKind>,
    pub scroll: u16,
}

#[derive(Clone)]
pub enum LineKind {
    User(String),
    Assistant(String),
    Reasoning(String),
    Tool(String),
    Output(String),
    Compacted(String),
    Status(String),
    Body(String),
    Blank,
}

pub fn detail_from_bytes(key: &str, bytes: &[u8], content_type: &str, version: i64) -> DetailView {
    if airctx_client::sessions::is_session_key(key) || content_type.contains("json") {
        let v = airctx_client::sessions::parse_session(bytes);
        if !v.events.is_empty() || !v.session_id.is_empty() {
            return detail_from_session(key, &v);
        }
    }
    if is_textlike(content_type) {
        let mut body = Vec::new();
        for (i, line) in String::from_utf8_lossy(bytes).lines().enumerate() {
            if i >= 2000 {
                body.push(LineKind::Status(format!("… {} more lines", String::from_utf8_lossy(bytes).lines().count().saturating_sub(2000))));
                break;
            }
            body.push(LineKind::Body(truncate_line(line, 240)));
        }
        return DetailView {
            key: key.to_string(),
            title: key_info(key).0,
            meta: format!("v{version} · {content_type} · {}", human_size(bytes.len())),
            body,
            scroll: 0,
        };
    }
    DetailView {
        key: key.to_string(),
        title: key_info(key).0,
        meta: format!("v{version} · {content_type} · {}", human_size(bytes.len())),
        body: vec![LineKind::Status("[binary]".into())],
        scroll: 0,
    }
}

pub fn detail_from_session(key: &str, v: &SessionView) -> DetailView {
    let title = if v.title.is_empty() {
        airctx_client::sessions::session_title(key)
    } else {
        v.title.clone()
    };
    let trunc = if v.truncated { " · truncated" } else { "" };
    let model = if v.model.is_empty() { String::new() } else { format!(" · {}", v.model) };
    let cwd = if v.cwd.is_empty() { String::new() } else { format!(" · {}", v.cwd) };
    let meta = format!(
        "{title}{cwd}{model} · {} events · {}{trunc}",
        v.events.len(),
        v.started.replace('T', " ").chars().take(19).collect::<String>()
    );
    let mut body = Vec::new();
    for ev in &v.events {
        match ev.kind {
            EvKind::User => {
                body.push(LineKind::User(format!("{} you", ev.kind.icon())));
                push_wrapped(&mut body, &ev.text, LineKind::Body, 120, 80);
            }
            EvKind::Assistant => {
                body.push(LineKind::Assistant(format!("{} {}", ev.kind.icon(), ev.kind.label())));
                push_wrapped(&mut body, &ev.text, LineKind::Body, 120, 80);
            }
            EvKind::Reasoning => {
                body.push(LineKind::Reasoning(format!("{} thinking", ev.kind.icon())));
                push_capped(&mut body, &ev.text, 12);
            }
            EvKind::ToolCall => {
                body.push(LineKind::Tool(format!("{} [{}]", ev.kind.icon(), ev.tool)));
                push_capped(&mut body, &ev.text, 16);
            }
            EvKind::ToolOutput => {
                body.push(LineKind::Output(format!("{} output", ev.kind.icon())));
                push_capped(&mut body, &ev.text, 20);
            }
            EvKind::Compacted => {
                body.push(LineKind::Compacted(format!("{} compacted", ev.kind.icon())));
                push_wrapped(&mut body, &ev.text, LineKind::Body, 100, 40);
            }
            EvKind::Status => {
                body.push(LineKind::Status(format!("{} {}", ev.kind.icon(), ev.text)));
            }
        }
        body.push(LineKind::Blank);
    }
    DetailView { key: key.to_string(), title, meta, body, scroll: 0 }
}

fn push_capped(body: &mut Vec<LineKind>, text: &str, max_lines: usize) {
    let total = text.lines().count();
    for (i, l) in text.lines().enumerate() {
        if i >= max_lines {
            body.push(LineKind::Status(format!("  … {} more lines", total.saturating_sub(max_lines))));
            break;
        }
        body.push(LineKind::Body(format!("  {}", truncate_line(l, 200))));
    }
}

fn push_wrapped(body: &mut Vec<LineKind>, text: &str, _kind: fn(String) -> LineKind, width: usize, max_lines: usize) {
    let wrapped = wrap_text(text, width);
    let total = wrapped.len();
    for (i, l) in wrapped.into_iter().enumerate() {
        if i >= max_lines {
            body.push(LineKind::Status(format!("  … {} more lines", total.saturating_sub(max_lines))));
            break;
        }
        body.push(LineKind::Body(format!("  {l}")));
    }
}

pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut w = 0usize;
        for ch in para.chars() {
            let cw = UnicodeWidthStr::width(ch.to_string().as_str()).max(1);
            if w + cw > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
                w = 0;
            }
            line.push(ch);
            w += cw;
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

pub fn is_textlike(ct: &str) -> bool {
    ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("yaml")
        || ct.contains("javascript")
        || ct.contains("toml")
}

pub fn rel_time(ts: i64) -> String {
    let d = unix_now() - ts;
    if d < 0 {
        "just now".into()
    } else if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86400)
    }
}

pub fn fmt_interval(secs: u64) -> String {
    match secs {
        0 => "-".into(),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

pub fn human_size(n: usize) -> String {
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

pub fn truncate_line(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

pub fn next_interval(current: u64, dir: i32) -> u64 {
    let choices = airctx_client::config::INTERVAL_CHOICES;
    let pos = choices
        .iter()
        .position(|&x| x == current)
        .unwrap_or_else(|| {
            choices
                .iter()
                .position(|&x| x > current)
                .unwrap_or(choices.len() - 1)
        });
    if dir < 0 {
        choices[pos.saturating_sub(1)]
    } else {
        choices[(pos + 1).min(choices.len() - 1)]
    }
}
