//! Codex rollout 会话:jsonl → 完整可读事件流。
//!
//! 上传端把解析结果存成 JSON;TUI 既能读新 JSON,也能读旧的原始 jsonl。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvKind {
    User,
    Assistant,
    Reasoning,
    ToolCall,
    ToolOutput,
    Compacted,
    Status,
}

impl EvKind {
    pub fn icon(self) -> &'static str {
        match self {
            EvKind::User => "\u{1f464}",      // 👤
            EvKind::Assistant => "\u{2733}",   // ✳
            EvKind::Reasoning => "\u{1f9e0}",  // 🧠
            EvKind::ToolCall => "\u{1f527}",   // 🔧
            EvKind::ToolOutput => "\u{1f4e4}", // 📤
            EvKind::Compacted => "\u{1f4d1}",  // 📑
            EvKind::Status => "\u{2139}",      // ℹ
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EvKind::User => "you",
            EvKind::Assistant => "codex",
            EvKind::Reasoning => "thinking",
            EvKind::ToolCall => "tool",
            EvKind::ToolOutput => "output",
            EvKind::Compacted => "compacted",
            EvKind::Status => "status",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ev {
    pub kind: EvKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ts: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionView {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fork_id: String,
    #[serde(default)]
    pub started: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub originator: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cli_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<serde_json::Value>,
    #[serde(default)]
    pub events: Vec<Ev>,
    /// 为贴合单值上限而裁过 tool/reasoning 文本(用户与助手消息尽量保留)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// 远端 key 是否是一条 Codex 会话(新 JSON 或旧 jsonl)
pub fn is_session_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    (k.contains("/codex/") && k.ends_with(".json"))
        || (k.contains("/sessions/") && k.ends_with(".jsonl"))
        || (k.contains("/codex/") && k.ends_with(".jsonl"))
}

/// 列表标题:优先会话 id,旧路径仍用日期目录
pub fn session_title(key: &str) -> String {
    if let Some(name) = key.rsplit('/').next() {
        let stem = name
            .strip_suffix(".json")
            .or_else(|| name.strip_suffix(".jsonl"))
            .unwrap_or(name);
        if stem.starts_with("rollout-") {
            if let Some(pos) = key.find("/sessions/") {
                let rest = &key[pos + 10..];
                let parts: Vec<&str> = rest.splitn(4, '/').collect();
                if parts.len() >= 3 {
                    return format!("{}/{}/{}", parts[0], parts[1], parts[2]);
                }
            }
        }
        return stem.to_string();
    }
    key.to_string()
}

/// 解析已上传的 JSON,或原始 rollout jsonl(兼容旧数据)
pub fn parse_session(bytes: &[u8]) -> SessionView {
    let s = String::from_utf8_lossy(bytes);
    let trimmed = s.trim_start();
    if trimmed.starts_with('{') {
        if let Ok(view) = serde_json::from_str::<SessionView>(trimmed) {
            if !view.events.is_empty() || !view.session_id.is_empty() {
                return view;
            }
        }
    }
    parse_jsonl(trimmed.lines())
}

pub fn parse_jsonl<'a, I: Iterator<Item = &'a str>>(lines: I) -> SessionView {
    let mut view = SessionView::default();
    for line in lines {
        ingest_raw(&mut view, line);
    }
    view
}

pub fn parse_jsonl_reader<R: std::io::BufRead>(reader: R) -> SessionView {
    let mut view = SessionView::default();
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        ingest_raw(&mut view, &line);
    }
    view
}

fn ingest_raw(view: &mut SessionView, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
    ingest_line(view, &v);
}

fn ingest_line(view: &mut SessionView, v: &serde_json::Value) {
    let ts = v.get("timestamp").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let payload = v.get("payload");
    match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "session_meta" => {
            let Some(p) = payload else { return };
            if view.session_id.is_empty() {
                view.session_id = str_field(p, "session_id")
                    .or_else(|| str_field(p, "id"))
                    .unwrap_or_default();
            }
            if view.started.is_empty() {
                view.started = str_field(p, "timestamp").unwrap_or_else(|| ts.clone());
            }
            if view.cwd.is_empty() {
                view.cwd = str_field(p, "cwd").unwrap_or_default();
            }
            if view.originator.is_empty() {
                view.originator = str_field(p, "originator").unwrap_or_default();
            }
            if view.source.is_empty() {
                view.source = str_field(p, "source").unwrap_or_default();
            }
            if view.cli_version.is_empty() {
                view.cli_version = str_field(p, "cli_version").unwrap_or_default();
            }
            if view.git.is_none() {
                view.git = p.get("git").cloned().filter(|g| !g.is_null());
            }
        }
        "turn_context" => {
            let Some(p) = payload else { return };
            if let Some(cwd) = str_field(p, "cwd") {
                view.cwd = cwd;
            }
            if let Some(model) = str_field(p, "model") {
                view.model = model;
            }
        }
        "compacted" => {
            let msg = payload.and_then(|p| str_field(p, "message")).unwrap_or_default();
            if !msg.is_empty() {
                view.events.push(Ev { kind: EvKind::Compacted, tool: String::new(), text: msg, ts });
            }
        }
        "event_msg" => {
            let Some(p) = payload else { return };
            match p.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "task_complete" => {
                    let text = str_field(p, "last_agent_message")
                        .or_else(|| str_field(p, "reason"))
                        .unwrap_or_else(|| "task complete".into());
                    view.events.push(Ev { kind: EvKind::Status, tool: String::new(), text, ts });
                }
                "turn_aborted" => {
                    let text = str_field(p, "reason").unwrap_or_else(|| "turn aborted".into());
                    view.events.push(Ev { kind: EvKind::Status, tool: String::new(), text, ts });
                }
                _ => {}
            }
        }
        "world_state" => {
            if view.cwd.is_empty() {
                if let Some(cwd) = payload
                    .and_then(|p| p.pointer("/state/environments/environments/local/cwd"))
                    .and_then(|x| x.as_str())
                {
                    view.cwd = cwd.to_string();
                }
            }
        }
        "response_item" => {
            let Some(p) = payload else { return };
            match p.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                "message" => {
                    let role = p.get("role").and_then(|r| r.as_str()).unwrap_or("");
                    let text = join_texts(p.get("content"));
                    if text.is_empty() || is_injected(role, &text) {
                        return;
                    }
                    view.events.push(Ev {
                        kind: if role == "user" { EvKind::User } else { EvKind::Assistant },
                        tool: String::new(),
                        text,
                        ts,
                    });
                }
                "reasoning" => {
                    let text = join_texts(p.get("summary"));
                    if !text.is_empty() {
                        view.events.push(Ev { kind: EvKind::Reasoning, tool: String::new(), text, ts });
                    }
                }
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    let tool = str_field(p, "name").unwrap_or_else(|| "tool".into());
                    let detail = tool_input(p);
                    view.events.push(Ev { kind: EvKind::ToolCall, tool, text: detail, ts });
                }
                "function_call_output" | "custom_tool_call_output" | "local_shell_call_output" => {
                    let text = output_text(p.get("output"));
                    if !text.is_empty() {
                        view.events.push(Ev { kind: EvKind::ToolOutput, tool: String::new(), text, ts });
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn str_field(v: &serde_json::Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

fn is_injected(role: &str, text: &str) -> bool {
    if role == "developer" {
        return true;
    }
    role == "user"
        && (text.starts_with("# AGENTS.md")
            || text.starts_with("<user_instructions>")
            || text.starts_with("<environment_context>")
            || text.starts_with("<app-context>")
            || text.starts_with("<ENVIRONMENT_"))
}

fn join_texts(v: Option<&serde_json::Value>) -> String {
    let Some(v) = v else { return String::new() };
    match v {
        serde_json::Value::Array(arr) => {
            let mut parts = Vec::new();
            for c in arr {
                if let Some(t) = c.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                } else if c.get("type").and_then(|t| t.as_str()) == Some("input_image") {
                    let loc = c
                        .get("image_url")
                        .or_else(|| c.get("url"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    parts.push(format!("[image {loc}]"));
                }
            }
            parts.join("\n")
        }
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn output_text(v: Option<&serde_json::Value>) -> String {
    let Some(v) = v else { return String::new() };
    match v {
        serde_json::Value::Array(_) => join_texts(Some(v)),
        serde_json::Value::String(s) => {
            let t = s.trim();
            if (t.starts_with('[') || t.starts_with('{')) && serde_json::from_str::<serde_json::Value>(t).is_ok() {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(t) {
                    let inner = output_text(Some(&parsed));
                    if !inner.is_empty() {
                        return inner;
                    }
                }
            }
            s.clone()
        }
        serde_json::Value::Object(map) => {
            if let Some(t) = map.get("text").and_then(|x| x.as_str()) {
                t.to_string()
            } else {
                v.to_string()
            }
        }
        _ => v.to_string(),
    }
}

fn tool_input(p: &serde_json::Value) -> String {
    if let Some(a) = p.get("arguments") {
        return value_as_text(a);
    }
    if let Some(i) = p.get("input") {
        return value_as_text(i);
    }
    if let Some(c) = p.get("command") {
        return value_as_text(c);
    }
    if let Some(params) = p.get("params") {
        return value_as_text(params);
    }
    String::new()
}

fn value_as_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 服务端单值上限 16MB;上传前把超大 tool/reasoning 文本收进去,对话本身不丢。
pub const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024 - 4096;

pub fn encode_for_upload(mut view: SessionView) -> (Vec<u8>, SessionView) {
    let caps: [Option<usize>; 6] = [
        None,
        Some(64 * 1024),
        Some(16 * 1024),
        Some(4 * 1024),
        Some(1024),
        Some(256),
    ];
    for cap in caps {
        if let Some(n) = cap {
            for ev in &mut view.events {
                if matches!(ev.kind, EvKind::ToolCall | EvKind::ToolOutput | EvKind::Reasoning | EvKind::Compacted)
                    && ev.text.len() > n
                {
                    ev.text = truncate_bytes(&ev.text, n);
                    view.truncated = true;
                }
            }
        }
        if let Ok(bytes) = serde_json::to_vec(&view) {
            if bytes.len() <= MAX_UPLOAD_BYTES {
                return (bytes, view);
            }
        }
    }

    view.events.retain(|e| e.kind != EvKind::Reasoning);
    view.truncated = true;
    if let Ok(bytes) = serde_json::to_vec(&view) {
        if bytes.len() <= MAX_UPLOAD_BYTES {
            return (bytes, view);
        }
    }

    while serde_json::to_vec(&view).map(|b| b.len()).unwrap_or(0) > MAX_UPLOAD_BYTES {
        let idx = view
            .events
            .iter()
            .position(|e| e.kind == EvKind::ToolOutput)
            .or_else(|| view.events.iter().position(|e| e.kind == EvKind::ToolCall))
            .or_else(|| view.events.iter().position(|e| e.kind == EvKind::Compacted));
        match idx {
            Some(i) => {
                view.events.remove(i);
            }
            None => break,
        }
    }
    let bytes = serde_json::to_vec(&view).unwrap_or_else(|_| b"{}".to_vec());
    (bytes, view)
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(24);
    let mut end = keep.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[truncated {}B]", &s[..end], s.len().saturating_sub(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
{"timestamp":"2026-09-17T00:00:00Z","type":"session_meta","payload":{"session_id":"abc-123","cwd":"/tmp/proj","originator":"cli","cli_version":"1.0","source":"cli","git":{"branch":"main"}}}
{"timestamp":"2026-09-17T00:00:01Z","type":"turn_context","payload":{"cwd":"/tmp/proj","model":"gpt-5"}}
{"timestamp":"2026-09-17T00:00:02Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<app-context>ignore"}]}}
{"timestamp":"2026-09-17T00:00:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello world"}]}}
{"timestamp":"2026-09-17T00:00:04Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"thinking about it"}]}}
{"timestamp":"2026-09-17T00:00:05Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"ls -la"}}
{"timestamp":"2026-09-17T00:00:06Z","type":"response_item","payload":{"type":"custom_tool_call_output","output":"[{\"type\":\"input_text\",\"text\":\"total 4\\nREADME.md\"}]"}}
{"timestamp":"2026-09-17T00:00:07Z","type":"response_item","payload":{"type":"function_call","name":"wait","arguments":"{\"n\":1}"}}
{"timestamp":"2026-09-17T00:00:08Z","type":"response_item","payload":{"type":"function_call_output","output":"ok"}}
{"timestamp":"2026-09-17T00:00:09Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}}
{"timestamp":"2026-09-17T00:00:10Z","type":"compacted","payload":{"message":"summary of prior work"}}
{"timestamp":"2026-09-17T00:00:11Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"all good"}}
{"timestamp":"2026-09-17T00:00:12Z","type":"event_msg","payload":{"type":"item_completed"}}
{"timestamp":"2026-09-17T00:00:13Z","type":"token_usage_record","payload":{}}
"#;

    #[test]
    fn parses_full_event_set() {
        let v = parse_session(FIXTURE.as_bytes());
        assert_eq!(v.session_id, "abc-123");
        assert_eq!(v.cwd, "/tmp/proj");
        assert_eq!(v.model, "gpt-5");
        assert_eq!(v.git.as_ref().unwrap()["branch"], "main");
        let kinds: Vec<_> = v.events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EvKind::User,
                EvKind::Reasoning,
                EvKind::ToolCall,
                EvKind::ToolOutput,
                EvKind::ToolCall,
                EvKind::ToolOutput,
                EvKind::Assistant,
                EvKind::Compacted,
                EvKind::Status,
            ]
        );
        assert_eq!(v.events[0].text, "hello world");
        assert_eq!(v.events[2].tool, "exec");
        assert!(v.events[3].text.contains("README.md"), "tool output should unpack json array: {}", v.events[3].text);
        assert_eq!(v.events[6].text, "done");
        assert_eq!(v.events[7].text, "summary of prior work");
        assert_eq!(v.events[8].text, "all good");
        assert!(!kinds.contains(&EvKind::User) || v.events.iter().all(|e| !e.text.contains("<app-context>")));
    }

    #[test]
    fn roundtrip_json_upload() {
        let v = parse_session(FIXTURE.as_bytes());
        let (bytes, _) = encode_for_upload(v.clone());
        let back = parse_session(&bytes);
        assert_eq!(back.session_id, "abc-123");
        assert_eq!(back.events.len(), v.events.len());
    }

    #[test]
    fn encode_fits_limit_by_truncating_tools() {
        let mut v = SessionView { session_id: "x".into(), ..Default::default() };
        v.events.push(Ev { kind: EvKind::User, tool: String::new(), text: "keep me".into(), ts: String::new() });
        v.events.push(Ev {
            kind: EvKind::ToolOutput,
            tool: String::new(),
            text: "Z".repeat(80 * 1024 * 1024),
            ts: String::new(),
        });
        v.events.push(Ev { kind: EvKind::Assistant, tool: String::new(), text: "also keep".into(), ts: String::new() });
        let (bytes, out) = encode_for_upload(v);
        assert!(bytes.len() <= MAX_UPLOAD_BYTES, "encoded {} bytes", bytes.len());
        assert!(out.truncated);
        assert!(out.events.iter().any(|e| e.kind == EvKind::User && e.text == "keep me"));
        assert!(out.events.iter().any(|e| e.kind == EvKind::Assistant && e.text == "also keep"));
    }

    #[test]
    fn session_key_detects_new_and_old() {
        assert!(is_session_key("devices/mac/codex/abc-123.json"));
        assert!(is_session_key("devices/mac/codex/sessions/2026/09/17/rollout-x.jsonl"));
        assert!(!is_session_key("notes/a.md"));
    }
}
