use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use super::format::{self, LineKind};
use super::{Focus, Screen, SettingsCursor, App};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const ERR: Color = Color::Red;
const HL: Color = Color::Rgb(36, 42, 54);

pub fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    match &app.screen {
        Screen::Login => draw_login(app, f, area),
        Screen::Help => {
            draw_browse(app, f, area);
            draw_help(f, area);
        }
        Screen::Settings => draw_settings(app, f, area),
        Screen::ConfirmDelete => {
            if app.detail.is_some() {
                draw_detail(app, f, area);
            } else {
                draw_browse(app, f, area);
            }
            draw_confirm(app, f, area);
        }
        Screen::Detail => draw_detail(app, f, area),
        Screen::Devices | Screen::Clients | Screen::Contexts => draw_browse(app, f, area),
    }
}

fn chunks_header_main_footer(area: Rect) -> (Rect, Rect, Rect) {
    let v = Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).split(area);
    (v[0], v[1], v[2])
}

fn draw_login(app: &App, f: &mut Frame, area: Rect) {
    let (header, main, footer) = chunks_header_main_footer(area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(" airctx", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))),
        header,
    );

    let box_area = centered(main, 64, 12);
    let url_mark = if app.login_field == 0 { "▸" } else { " " };
    let tok_mark = if app.login_field == 1 { "▸" } else { " " };
    let url = if app.login_field == 0 {
        format!("{}█", app.login_url)
    } else {
        app.login_url.clone()
    };
    let token = if app.login_token.is_empty() {
        String::new()
    } else {
        "•".repeat(app.login_token.chars().count().min(48))
    };
    let token_disp = if app.login_field == 1 {
        format!("{token}█")
    } else {
        token
    };
    let mut lines = vec![
        Line::from(Span::styled(" Connect", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(" Security URL from the server, then token from the web UI", Style::default().fg(DIM))),
        Line::from(""),
        Line::from(format!(" {url_mark} url    {url}")),
        Line::from(format!(" {tok_mark} token  {token_disp}")),
        Line::from(""),
        Line::from(Span::styled(" e.g. http://1.2.3.4:8787/a8k3mpx2nq4r", Style::default().fg(DIM))),
    ];
    if let Some((msg, _, is_err)) = &app.toast {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            msg.clone(),
            Style::default().fg(if *is_err { ERR } else { OK }),
        )));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ACCENT))),
        box_area,
    );
    footer_line(f, footer, "tab field   enter connect   q quit");
}

fn draw_browse(app: &mut App, f: &mut Frame, area: Rect) {
    let (header, main, footer) = chunks_header_main_footer(area);
    draw_header(app, f, header);
    match app.screen {
        Screen::Devices => draw_devices(app, f, main),
        Screen::Clients => draw_clients(app, f, main),
        Screen::Contexts => draw_contexts(app, f, main),
        _ => draw_devices(app, f, main),
    }
    footer_line(f, footer, &browse_footer(app));
}

fn browse_footer(app: &App) -> String {
    if app.focus == Focus::Command {
        return format!(":{}█   enter run   esc cancel   :ls  :devices  :codex", app.command);
    }
    if app.focus == Focus::Filter {
        return format!("/{}█   enter apply   esc cancel", app.filter_draft);
    }
    match app.screen {
        Screen::Devices => "j/k move  enter open  : command  :ls list  s sync  e settings  ? help  q quit".into(),
        Screen::Clients => "j/k move  enter sessions  esc devices  :ls  s sync  e settings  q quit".into(),
        Screen::Contexts => {
            if !app.filter.is_empty() {
                format!("filter: {}   / edit   esc back   enter open   d delete", app.filter)
            } else {
                "j/k move  enter open  / find  esc back  d delete  :ls  s sync  q quit".into()
            }
        }
        _ => String::new(),
    }
}

fn draw_devices(app: &mut App, f: &mut Frame, area: Rect) {
    let rows = app.device_rows();
    let n = rows.len();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|d| {
            let tag = if d.is_local { "  this machine" } else { "" };
            let when = d.last_updated.map(format::rel_time).unwrap_or_else(|| "—".into());
            ListItem::new(Line::from(vec![
                Span::styled(format!("  {}", d.name), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(tag.to_string(), Style::default().fg(ACCENT)),
                Span::styled(format!("   {} sessions · {when}", d.count), Style::default().fg(DIM)),
            ]))
        })
        .collect();
    let title = format!(" devices ({n}) ");
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ACCENT)).title(title))
        .highlight_style(Style::default().bg(HL).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut app.list_state);
    if n == 0 {
        empty_hint(f, area, "no devices yet — press s to start sync, or :ls to list all context");
    }
}

fn draw_clients(app: &mut App, f: &mut Frame, area: Rect) {
    let rows = app.client_rows();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|c| {
            let status = if c.count == 0 {
                "no sessions yet".to_string()
            } else {
                let when = c.last_updated.map(format::rel_time).unwrap_or_default();
                format!("{} sessions · {when}", c.count)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("  {:<14}", c.display), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  {status}"), Style::default().fg(if c.count == 0 { DIM } else { OK })),
            ]))
        })
        .collect();
    let title = format!(" {} ", app.breadcrumb());
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ACCENT)).title(title))
        .highlight_style(Style::default().bg(HL).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_contexts(app: &mut App, f: &mut Frame, area: Rect) {
    let width = area.width.saturating_sub(4) as usize;
    let (items, n) = {
        let rows = app.visible_items();
        let n = rows.len();
        let items: Vec<ListItem> = rows
            .iter()
            .map(|it| {
                let (title, sub) = format::key_info(&it.key);
                let meta = format!("{} · {}", format::rel_time(it.updated_at), format::human_size(it.size));
                let title_d = format::truncate_line(&title, width.saturating_sub(meta.len() + 8).max(8));
                let sub_d = format::truncate_line(&sub, 16);
                ListItem::new(Line::from(vec![
                    Span::styled(title_d, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {sub_d}"), Style::default().fg(DIM)),
                    Span::raw("  "),
                    Span::styled(meta, Style::default().fg(DIM)),
                ]))
            })
            .collect();
        (items, n)
    };
    let title = format!(" {} · {n} ", app.breadcrumb());
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ACCENT)).title(title))
        .highlight_style(Style::default().bg(HL).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    f.render_stateful_widget(list, area, &mut app.list_state);
    if n == 0 {
        empty_hint(f, area, "no sessions here — esc back, or :ls after picking another client");
    }
}

fn empty_hint(f: &mut Frame, area: Rect, hint: &str) {
    let inner = Rect {
        x: area.x + 2,
        y: area.y + area.height / 2,
        width: area.width.saturating_sub(4),
        height: 1,
    };
    f.render_widget(Paragraph::new(Span::styled(hint, Style::default().fg(DIM))), inner);
}

fn draw_header(app: &App, f: &mut Frame, area: Rect) {
    let mut spans = vec![Span::styled(" airctx ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))];
    if app.daemon.running {
        spans.push(Span::styled("●", Style::default().fg(OK)));
        spans.push(Span::raw(format!(" sync ↑{} ", app.daemon.uploads_ok)));
        if app.daemon.uploads_failed > 0 {
            spans.push(Span::styled(format!("✗{} ", app.daemon.uploads_failed), Style::default().fg(ERR)));
        }
        if let Some(t) = app.daemon.last_scan_at {
            spans.push(Span::styled(format!("· {} ", format::rel_time(t)), Style::default().fg(DIM)));
        }
    } else {
        spans.push(Span::styled("○ sync off", Style::default().fg(WARN)));
        spans.push(Span::styled("  press s to start  ", Style::default().fg(DIM)));
    }
    if let Some(cfg) = &app.cfg {
        let device = cfg.daemon.device_name.as_deref().unwrap_or("device");
        spans.push(Span::styled(format!("· {device} · {}", cfg.server_url), Style::default().fg(DIM)));
    }
    if let Some((msg, _, is_err)) = &app.toast {
        if msg.len() < 40 {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                msg.clone(),
                Style::default().fg(if *is_err { ERR } else { ACCENT }),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_detail(app: &App, f: &mut Frame, area: Rect) {
    let (header, main, footer) = chunks_header_main_footer(area);
    draw_header(app, f, header);
    let Some(d) = &app.detail else { return };
    let lines: Vec<Line> = d
        .body
        .iter()
        .map(|lk| match lk {
            LineKind::User(s) => Line::from(Span::styled(s.clone(), Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD))),
            LineKind::Assistant(s) => Line::from(Span::styled(s.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))),
            LineKind::Reasoning(s) => Line::from(Span::styled(s.clone(), Style::default().fg(DIM))),
            LineKind::Tool(s) => Line::from(Span::styled(s.clone(), Style::default().fg(Color::LightMagenta))),
            LineKind::Output(s) => Line::from(Span::styled(s.clone(), Style::default().fg(DIM))),
            LineKind::Compacted(s) => Line::from(Span::styled(s.clone(), Style::default().fg(WARN))),
            LineKind::Status(s) => Line::from(Span::styled(s.clone(), Style::default().fg(DIM))),
            LineKind::Body(s) => Line::from(s.clone()),
            LineKind::Blank => Line::from(""),
        })
        .collect();
    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(format!(" {} ", format::truncate_line(&d.title, 40)))
                .title(Span::styled(format!(" {} ", format::truncate_line(&d.meta, 60)), Style::default().fg(DIM))),
        )
        .scroll((d.scroll, 0));
    f.render_widget(p, main);
    footer_line(f, footer, "j/k scroll  g/G top/end  esc back  d delete");
}

fn draw_settings(app: &App, f: &mut Frame, area: Rect) {
    let (header, main, footer) = chunks_header_main_footer(area);
    draw_header(app, f, header);
    let Some(cfg) = &app.cfg else { return };

    let mut lines: Vec<Line> = Vec::new();
    let sel = |i: SettingsCursor| {
        if app.settings_cursor == i {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };
    let mark = |i: SettingsCursor| if app.settings_cursor == i { "▸ " } else { "  " };

    lines.push(Line::from(Span::styled(" Settings", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("{}device     {}", mark(SettingsCursor::Device), cfg.daemon.device_name.as_deref().unwrap_or("(auto)")),
        sel(SettingsCursor::Device),
    )));
    let daemon = if app.daemon.running {
        format!("running  pid {}", app.daemon.pid)
    } else {
        "stopped".into()
    };
    lines.push(Line::from(Span::styled(
        format!("{}sync       {daemon}", mark(SettingsCursor::Daemon)),
        sel(SettingsCursor::Daemon),
    )));
    let auto = match app.autostart {
        Some(true) => "on",
        Some(false) => "off",
        None => "n/a",
    };
    lines.push(Line::from(Span::styled(
        format!("{}autostart  {auto}", mark(SettingsCursor::Autostart)),
        sel(SettingsCursor::Autostart),
    )));
    lines.push(Line::from(Span::styled(
        format!("{}interval   {}", mark(SettingsCursor::Interval), format::fmt_interval(cfg.daemon.interval_secs)),
        sel(SettingsCursor::Interval),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" Sources", Style::default().fg(DIM))));
    let device = cfg.daemon.device_name.clone().unwrap_or_default();
    for st in airctx_client::sources::source_states(cfg, &device) {
        let state = if st.prefix.is_some() {
            "syncing"
        } else if !st.enabled {
            "off"
        } else {
            "not found"
        };
        lines.push(Line::from(Span::styled(
            format!("  {}  {state}  {}", st.id, st.root),
            Style::default().fg(DIM),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(" Watched folders  (a add, x remove)", Style::default().fg(DIM))));
    if cfg.daemon.watch.is_empty() {
        lines.push(Line::from(Span::styled("  (none — Codex is picked up automatically)", Style::default().fg(DIM))));
    }
    for (i, w) in cfg.daemon.watch.iter().enumerate() {
        let cur = SettingsCursor::Watch(i);
        lines.push(Line::from(Span::styled(
            format!("{}{} → {}", mark(cur), w.path, w.prefix),
            sel(cur),
        )));
    }

    if let Some(edit) = &app.settings_edit {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(format!(" typing: {edit}█"), Style::default().fg(ACCENT))));
    }

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ACCENT)))
            .wrap(Wrap { trim: false }),
        main,
    );

    let foot = if app.settings_edit.is_some() {
        "enter save   esc cancel"
    } else {
        "j/k move  enter edit/toggle  ←→ interval  a add folder  x remove  esc back"
    };
    footer_line(f, footer, foot);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let popup = centered(area, 72, 22);
    let lines = vec![
        Line::from(Span::styled(" Browse", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from("  devices → Codex / Claude Code / OpenCode → sessions"),
        Line::from("  enter           go in"),
        Line::from("  esc             go back"),
        Line::from("  :ls             show session list (hidden by default)"),
        Line::from("  :devices        device list"),
        Line::from("  :codex          this machine's Codex sessions"),
        Line::from(""),
        Line::from(Span::styled(" Always", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
        Line::from("  j k             move"),
        Line::from("  s               start / run sync"),
        Line::from("  e               settings"),
        Line::from("  /               find (on session list)"),
        Line::from("  d               delete session"),
        Line::from("  q               quit"),
        Line::from(""),
        Line::from(Span::styled(" esc closes help", Style::default().fg(DIM))),
    ];
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ACCENT)).title(" help ")),
        popup,
    );
}

fn draw_confirm(app: &App, f: &mut Frame, area: Rect) {
    let popup = centered(area, 56, 7);
    let key = app.pending_delete.clone().unwrap_or_default();
    let lines = vec![
        Line::from(Span::styled(" Delete this context?", Style::default().fg(ERR).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(Span::styled(format::truncate_line(&key, 50), Style::default().fg(Color::White))),
        Line::from(""),
        Line::from(Span::styled(" y yes    n / esc cancel", Style::default().fg(DIM))),
    ];
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(ERR))),
        popup,
    );
}

fn footer_line(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text}"), Style::default().fg(DIM)))),
        area,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 3,
        width: w,
        height: h,
    }
}
