//! 交互式 TUI:默认设备页 → 设备内客户端 → 上下文列表(命令 :ls 或进入客户端后才出现)。

mod format;
mod view;

use anyhow::{bail, Result};
use std::io::IsTerminal;
use std::time::{Duration, Instant};

use airctx_client::config::{self, Config, WatchEntry};
use airctx_client::http::Client;
use airctx_client::ipc;
use airctx_core::store::ListItem as CtxItem;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use format::{self as fmt, DetailView};

const POLL: Duration = Duration::from_millis(200);
const DAEMON_POLL: Duration = Duration::from_secs(2);
const LIST_POLL: Duration = Duration::from_secs(5);
const TOAST: Duration = Duration::from_secs(4);
const LIST_LIMIT: u32 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Devices,
    Clients,
    Contexts,
    Detail,
    Settings,
    Help,
    ConfirmDelete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Filter,
    Command,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsCursor {
    Device,
    Daemon,
    Autostart,
    Interval,
    Watch(usize),
}

pub struct DaemonInfo {
    pub running: bool,
    pub pid: u64,
    pub uploads_ok: u64,
    pub uploads_failed: u64,
    pub last_scan_at: Option<i64>,
}

impl DaemonInfo {
    fn off() -> Self {
        Self { running: false, pid: 0, uploads_ok: 0, uploads_failed: 0, last_scan_at: None }
    }
}

pub struct App {
    pub screen: Screen,
    pub cfg: Option<Config>,
    client: Option<Client>,
    items: Vec<CtxItem>,
    pub list_state: ListState,
    pub focus: Focus,
    pub filter: String,
    pub filter_draft: String,
    pub command: String,
    pub nav_device: Option<String>,
    pub nav_client: Option<String>,
    pub return_screen: Screen,
    pub detail: Option<DetailView>,
    pub daemon: DaemonInfo,
    pub autostart: Option<bool>,
    pub toast: Option<(String, Instant, bool)>,
    should_quit: bool,
    last_daemon_poll: Instant,
    last_list_poll: Instant,
    pub login_url: String,
    pub login_token: String,
    pub login_field: u8,
    pub settings_cursor: SettingsCursor,
    pub settings_edit: Option<String>,
    pub pending_delete: Option<String>,
}

impl App {
    fn new(cfg: Option<Config>) -> Self {
        let mut app = Self {
            screen: if cfg.is_some() { Screen::Devices } else { Screen::Login },
            client: cfg.as_ref().map(Client::from_config_value),
            cfg,
            items: Vec::new(),
            list_state: ListState::default(),
            focus: Focus::List,
            filter: String::new(),
            filter_draft: String::new(),
            command: String::new(),
            nav_device: None,
            nav_client: None,
            return_screen: Screen::Devices,
            detail: None,
            daemon: DaemonInfo::off(),
            autostart: airctx_client::daemon::autostart_enabled(),
            toast: None,
            should_quit: false,
            last_daemon_poll: Instant::now() - DAEMON_POLL,
            last_list_poll: Instant::now() - LIST_POLL,
            login_url: String::new(),
            login_token: String::new(),
            login_field: 0,
            settings_cursor: SettingsCursor::Device,
            settings_edit: None,
            pending_delete: None,
        };
        if let Some(c) = &app.cfg {
            app.login_url = c.server_url.clone();
        }
        app.list_state.select(Some(0));
        app.refresh_list();
        app.poll_daemon();
        app
    }

    fn local_device(&self) -> Option<&str> {
        self.cfg.as_ref().and_then(|c| c.daemon.device_name.as_deref())
    }

    pub fn device_rows(&self) -> Vec<fmt::DeviceRow> {
        fmt::devices_from_items(&self.items, self.local_device())
    }

    pub fn client_rows(&self) -> Vec<fmt::ClientRow> {
        match &self.nav_device {
            Some(d) => fmt::clients_for_device(&self.items, d),
            None => Vec::new(),
        }
    }

    fn current_prefix(&self) -> String {
        fmt::context_prefix(self.nav_device.as_deref(), self.nav_client.as_deref())
    }

    pub fn visible_items(&self) -> Vec<&CtxItem> {
        let prefix = self.current_prefix();
        let mut v: Vec<&CtxItem> = self
            .items
            .iter()
            .filter(|i| fmt::matches_filter(i, &prefix, &self.filter))
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        v
    }

    pub fn breadcrumb(&self) -> String {
        match (&self.nav_device, &self.nav_client) {
            (Some(d), Some(c)) => format!("{d} / {}", airctx_client::sources::source_display(c)),
            (Some(d), None) => d.clone(),
            _ => "devices".into(),
        }
    }

    fn selected_key(&self) -> Option<String> {
        let sel = self.list_state.selected()?;
        self.visible_items().get(sel).map(|i| i.key.clone())
    }

    fn browse_len(&self) -> usize {
        match self.screen {
            Screen::Devices => self.device_rows().len(),
            Screen::Clients => self.client_rows().len(),
            Screen::Contexts => self.visible_items().len(),
            _ => self.visible_items().len(),
        }
    }

    fn clamp_list_sel(&mut self) {
        let n = self.browse_len();
        if n == 0 {
            self.list_state.select(None);
        } else {
            let i = self.list_state.selected().unwrap_or(0).min(n - 1);
            self.list_state.select(Some(i));
        }
    }

    fn reset_sel(&mut self) {
        let n = self.browse_len();
        self.list_state.select(if n == 0 { None } else { Some(0) });
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some((msg.into(), Instant::now(), err));
    }

    fn poll_daemon(&mut self) {
        match ipc::call_daemon("status", serde_json::Value::Null) {
            Ok(v) => {
                let st = v.get("status").cloned().unwrap_or(serde_json::Value::Null);
                self.daemon = DaemonInfo {
                    running: true,
                    pid: v.get("pid").and_then(|p| p.as_u64()).unwrap_or(0),
                    uploads_ok: st.get("uploads_ok").and_then(|t| t.as_u64()).unwrap_or(0),
                    uploads_failed: st.get("uploads_failed").and_then(|t| t.as_u64()).unwrap_or(0),
                    last_scan_at: st.get("last_scan_at").and_then(|t| t.as_i64()),
                };
            }
            Err(_) => self.daemon = DaemonInfo::off(),
        }
        self.autostart = airctx_client::daemon::autostart_enabled();
    }

    fn refresh_list(&mut self) {
        let Some(client) = &self.client else {
            self.items.clear();
            self.clamp_list_sel();
            return;
        };
        match client.list(None, LIST_LIMIT) {
            Ok(items) => {
                let keep = self.selected_key();
                self.items = items;
                if let Some(k) = keep {
                    let vis = self.visible_items();
                    if let Some(i) = vis.iter().position(|it| it.key == k) {
                        self.list_state.select(Some(i));
                    } else {
                        self.clamp_list_sel();
                    }
                } else {
                    self.clamp_list_sel();
                }
            }
            Err(e) => self.toast(format!("{e}"), true),
        }
    }

    fn open_selected(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let Some(client) = &self.client else { return };
        match client.get(&key) {
            Ok(res) => {
                self.detail = Some(fmt::detail_from_bytes(&key, &res.bytes, &res.content_type, res.version));
                self.screen = Screen::Detail;
            }
            Err(e) => self.toast(format!("{e}"), true),
        }
    }

    fn save_cfg(&mut self) {
        let Some(cfg) = &self.cfg else { return };
        if let Err(e) = config::save(cfg) {
            self.toast(format!("save failed: {e}"), true);
            return;
        }
        if ipc::daemon_running() {
            if let Err(e) = ipc::call_daemon("reload", serde_json::Value::Null) {
                self.toast(format!("saved, daemon: {e}"), true);
            }
        }
    }

    fn do_sync(&mut self) {
        if self.daemon.running {
            match ipc::call_daemon("sync_now", serde_json::Value::Null) {
                Ok(_) => self.toast("syncing…", false),
                Err(e) => self.toast(format!("{e}"), true),
            }
        } else {
            match airctx_client::daemon::start() {
                Ok(()) => {
                    self.poll_daemon();
                    self.toast("sync started", false);
                }
                Err(e) => self.toast(format!("{e}"), true),
            }
        }
    }

    fn connect(&mut self) {
        let url = config::normalize_url(&self.login_url);
        let token = self.login_token.trim().to_string();
        if url.is_empty() || token.is_empty() {
            self.toast("security url + token from web UI required", true);
            return;
        }
        let client = Client::new(url.clone(), token.clone());
        if let Err(e) = client.list(None, 1) {
            self.toast(format!("cannot connect: {e}"), true);
            return;
        }
        match config::save_login(&url, &token) {
            Ok(cfg) => {
                self.cfg = Some(cfg.clone());
                self.client = Some(Client::from_config_value(&cfg));
                self.screen = Screen::Devices;
                self.focus = Focus::List;
                self.toast(format!("connected to {url}"), false);
                self.refresh_list();
                self.poll_daemon();
            }
            Err(e) => self.toast(format!("save failed: {e}"), true),
        }
    }

    fn delete_key(&mut self, key: &str) {
        let Some(client) = &self.client else { return };
        match client.delete(key) {
            Ok(_) => {
                self.toast("deleted", false);
                self.detail = None;
                self.screen = Screen::Contexts;
                self.refresh_list();
            }
            Err(e) => self.toast(format!("{e}"), true),
        }
    }

    fn event_loop(mut self, terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>) -> Result<()> {
        while !self.should_quit {
            if let Some((_, at, _)) = &self.toast {
                if at.elapsed() > TOAST {
                    self.toast = None;
                }
            }
            terminal.draw(|f| view::draw(&mut self, f))?;
            if crossterm::event::poll(POLL)? {
                if let Event::Key(key) = crossterm::event::read()? {
                    if key.kind == KeyEventKind::Press {
                        self.on_key(key.code, key.modifiers);
                    }
                }
            }
            if self.screen != Screen::Login && self.last_daemon_poll.elapsed() >= DAEMON_POLL {
                self.poll_daemon();
                self.last_daemon_poll = Instant::now();
            }
            if matches!(self.screen, Screen::Devices | Screen::Clients | Screen::Contexts)
                && self.last_list_poll.elapsed() >= LIST_POLL
            {
                self.refresh_list();
                self.last_list_poll = Instant::now();
            }
        }
        Ok(())
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
            self.should_quit = true;
            return;
        }
        match self.screen {
            Screen::Login => self.on_key_login(code),
            Screen::Help => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')) {
                    self.screen = self.return_screen;
                }
            }
            Screen::ConfirmDelete => self.on_key_confirm(code),
            Screen::Detail => self.on_key_detail(code),
            Screen::Settings => self.on_key_settings(code),
            Screen::Devices | Screen::Clients | Screen::Contexts => self.on_key_browse(code),
        }
    }

    fn on_key_login(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab | KeyCode::Down => self.login_field = (self.login_field + 1) % 2,
            KeyCode::BackTab | KeyCode::Up => self.login_field = (self.login_field + 1) % 2,
            KeyCode::Enter => self.connect(),
            KeyCode::Backspace => {
                if self.login_field == 0 {
                    self.login_url.pop();
                } else {
                    self.login_token.pop();
                }
            }
            KeyCode::Char(c) => {
                if self.login_field == 0 {
                    self.login_url.push(c);
                } else {
                    self.login_token.push(c);
                }
            }
            _ => {}
        }
    }

    fn on_key_confirm(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(k) = self.pending_delete.take() {
                    self.delete_key(&k);
                }
                self.screen = Screen::Contexts;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.pending_delete = None;
                self.screen = if self.detail.is_some() { Screen::Detail } else { Screen::Contexts };
            }
            _ => {}
        }
    }

    fn on_key_detail(&mut self, code: KeyCode) {
        let Some(d) = self.detail.as_mut() else { return };
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.screen = Screen::Contexts;
            }
            KeyCode::Down | KeyCode::Char('j') => d.scroll = d.scroll.saturating_add(2),
            KeyCode::Up | KeyCode::Char('k') => d.scroll = d.scroll.saturating_sub(2),
            KeyCode::PageDown => d.scroll = d.scroll.saturating_add(12),
            KeyCode::PageUp => d.scroll = d.scroll.saturating_sub(12),
            KeyCode::Char('g') => d.scroll = 0,
            KeyCode::Char('G') => d.scroll = d.body.len().saturating_sub(1) as u16,
            KeyCode::Char('d') => {
                self.pending_delete = self.detail.as_ref().map(|d| d.key.clone());
                self.screen = Screen::ConfirmDelete;
            }
            _ => {}
        }
    }

    fn on_key_settings(&mut self, code: KeyCode) {
        if let Some(buf) = self.settings_edit.as_mut() {
            match code {
                KeyCode::Esc => self.settings_edit = None,
                KeyCode::Enter => self.commit_settings_edit(),
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => self.screen = self.return_screen,
            KeyCode::Down | KeyCode::Char('j') => self.settings_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.settings_move(-1),
            KeyCode::Left | KeyCode::Char('h') if self.settings_cursor == SettingsCursor::Interval => {
                self.nudge_interval(-1);
            }
            KeyCode::Right | KeyCode::Char('l') if self.settings_cursor == SettingsCursor::Interval => {
                self.nudge_interval(1);
            }
            KeyCode::Enter => self.settings_activate(),
            KeyCode::Char('a') => {
                self.settings_edit = Some(String::new());
                self.settings_cursor = SettingsCursor::Watch(self.cfg.as_ref().map(|c| c.daemon.watch.len()).unwrap_or(0));
                self.toast("path then prefix, e.g. ~/Notes notes/", false);
            }
            KeyCode::Char('x') => {
                if let SettingsCursor::Watch(i) = self.settings_cursor {
                    if let Some(cfg) = &mut self.cfg {
                        if i < cfg.daemon.watch.len() {
                            cfg.daemon.watch.remove(i);
                            self.save_cfg();
                            self.toast("watch removed", false);
                            self.settings_cursor = SettingsCursor::Interval;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn settings_move(&mut self, dir: i32) {
        let n_watch = self.cfg.as_ref().map(|c| c.daemon.watch.len()).unwrap_or(0);
        let max = 4 + n_watch; // 0..3 fixed, then watches
        let idx = match self.settings_cursor {
            SettingsCursor::Device => 0,
            SettingsCursor::Daemon => 1,
            SettingsCursor::Autostart => 2,
            SettingsCursor::Interval => 3,
            SettingsCursor::Watch(i) => 4 + i,
        } as i32;
        let next = (idx + dir).clamp(0, max.saturating_sub(1) as i32) as usize;
        self.settings_cursor = match next {
            0 => SettingsCursor::Device,
            1 => SettingsCursor::Daemon,
            2 => SettingsCursor::Autostart,
            3 => SettingsCursor::Interval,
            k => SettingsCursor::Watch(k - 4),
        };
    }

    fn settings_activate(&mut self) {
        match self.settings_cursor {
            SettingsCursor::Device => {
                let cur = self.cfg.as_ref().and_then(|c| c.daemon.device_name.clone()).unwrap_or_default();
                self.settings_edit = Some(cur);
            }
            SettingsCursor::Daemon => {
                if self.daemon.running {
                    let _ = airctx_client::daemon::stop();
                    self.poll_daemon();
                    self.toast("sync stopped", false);
                } else {
                    self.do_sync();
                }
            }
            SettingsCursor::Autostart => {
                let on = self.autostart.unwrap_or(false);
                let r = if on {
                    airctx_client::daemon::uninstall()
                } else {
                    airctx_client::daemon::install()
                };
                match r {
                    Ok(()) => {
                        self.poll_daemon();
                        self.toast(if on { "autostart off" } else { "autostart on" }, false);
                    }
                    Err(e) => self.toast(format!("{e}"), true),
                }
            }
            SettingsCursor::Interval => self.nudge_interval(1),
            SettingsCursor::Watch(_) => {}
        }
    }

    fn nudge_interval(&mut self, dir: i32) {
        let Some(cfg) = &mut self.cfg else { return };
        cfg.daemon.interval_secs = fmt::next_interval(cfg.daemon.interval_secs, dir);
        let v = cfg.daemon.interval_secs;
        self.save_cfg();
        self.toast(format!("interval {}", fmt::fmt_interval(v)), false);
    }

    fn commit_settings_edit(&mut self) {
        let Some(raw) = self.settings_edit.take() else { return };
        let raw = raw.trim().to_string();
        if raw.is_empty() {
            return;
        }
        match self.settings_cursor {
            SettingsCursor::Device => {
                if let Some(cfg) = &mut self.cfg {
                    cfg.daemon.device_name = Some(raw.clone());
                    self.save_cfg();
                    self.toast(format!("device {raw}"), false);
                }
            }
            SettingsCursor::Watch(_) => {
                let mut parts = raw.split_whitespace();
                let path = parts.next().unwrap_or("").to_string();
                let prefix = parts.next().unwrap_or("").to_string();
                if path.is_empty() || prefix.is_empty() {
                    self.toast("need: <dir> <prefix>", true);
                    return;
                }
                if let Some(cfg) = &mut self.cfg {
                    cfg.daemon.watch.push(WatchEntry { path, prefix });
                    self.save_cfg();
                    self.toast("folder added", false);
                }
            }
            _ => {}
        }
    }

    fn on_key_browse(&mut self, code: KeyCode) {
        if self.focus == Focus::Command {
            match code {
                KeyCode::Esc => {
                    self.focus = Focus::List;
                    self.command.clear();
                }
                KeyCode::Enter => {
                    let cmd = std::mem::take(&mut self.command);
                    self.focus = Focus::List;
                    self.run_command(&cmd);
                }
                KeyCode::Backspace => {
                    self.command.pop();
                }
                KeyCode::Char(c) => self.command.push(c),
                _ => {}
            }
            return;
        }
        if self.focus == Focus::Filter {
            match code {
                KeyCode::Esc => {
                    self.focus = Focus::List;
                    self.filter_draft.clear();
                }
                KeyCode::Enter => {
                    self.filter = self.filter_draft.clone();
                    self.focus = Focus::List;
                    self.clamp_list_sel();
                }
                KeyCode::Backspace => {
                    self.filter_draft.pop();
                }
                KeyCode::Char(c) => self.filter_draft.push(c),
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Char('e') => self.open_settings(),
            KeyCode::Char('s') => self.do_sync(),
            KeyCode::Char('r') => {
                self.refresh_list();
                self.toast("refreshed", false);
            }
            KeyCode::Char(':') => {
                self.focus = Focus::Command;
                self.command.clear();
            }
            KeyCode::Char('/') if self.screen == Screen::Contexts => {
                self.focus = Focus::Filter;
                self.filter_draft = self.filter.clone();
            }
            KeyCode::Esc => self.go_back(),
            KeyCode::Char('d') if self.screen == Screen::Contexts => {
                if let Some(k) = self.selected_key() {
                    self.pending_delete = Some(k);
                    self.screen = Screen::ConfirmDelete;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::Enter => self.drill_in(),
            _ => {}
        }
    }

    fn go_back(&mut self) {
        match self.screen {
            Screen::Contexts => {
                self.filter.clear();
                self.nav_client = None;
                self.screen = Screen::Clients;
                self.reset_sel();
            }
            Screen::Clients => {
                self.nav_device = None;
                self.screen = Screen::Devices;
                self.reset_sel();
            }
            Screen::Devices => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                }
            }
            _ => {}
        }
    }

    fn drill_in(&mut self) {
        match self.screen {
            Screen::Devices => {
                let rows = self.device_rows();
                let Some(i) = self.list_state.selected() else { return };
                let Some(row) = rows.get(i) else { return };
                self.nav_device = Some(row.name.clone());
                self.nav_client = None;
                self.screen = Screen::Clients;
                self.reset_sel();
            }
            Screen::Clients => {
                let rows = self.client_rows();
                let Some(i) = self.list_state.selected() else { return };
                let Some(row) = rows.get(i) else { return };
                self.nav_client = Some(row.id.clone());
                self.filter.clear();
                self.screen = Screen::Contexts;
                self.reset_sel();
            }
            Screen::Contexts => self.open_selected(),
            _ => {}
        }
    }

    fn open_help(&mut self) {
        self.return_screen = self.screen;
        self.screen = Screen::Help;
    }

    fn open_settings(&mut self) {
        self.return_screen = self.screen;
        self.settings_cursor = SettingsCursor::Device;
        self.screen = Screen::Settings;
    }

    fn run_command(&mut self, raw: &str) {
        let cmd = raw.trim().trim_start_matches('/').to_ascii_lowercase();
        let mut parts = cmd.split_whitespace();
        let Some(head) = parts.next() else { return };
        match head {
            "ls" | "list" | "contexts" => {
                if self.nav_device.is_none() {
                    if let Some(first) = parts.next() {
                        self.nav_device = Some(first.to_string());
                    }
                }
                if self.nav_client.is_none() {
                    if let Some(c) = parts.next() {
                        self.nav_client = Some(c.to_string());
                    }
                }
                self.filter.clear();
                self.screen = Screen::Contexts;
                self.reset_sel();
            }
            "devices" => {
                self.nav_device = None;
                self.nav_client = None;
                self.screen = Screen::Devices;
                self.reset_sel();
            }
            "codex" | "claude" | "claude-code" | "opencode" => {
                let id = match head {
                    "claude" => "claude-code",
                    other => other,
                };
                if self.nav_device.is_none() {
                    if let Some(local) = self.local_device() {
                        self.nav_device = Some(local.to_string());
                    } else if let Some(d) = self.device_rows().first() {
                        self.nav_device = Some(d.name.clone());
                    }
                }
                self.nav_client = Some(id.to_string());
                self.screen = Screen::Contexts;
                self.reset_sel();
            }
            "sync" | "s" => self.do_sync(),
            "settings" | "e" => self.open_settings(),
            "help" | "?" => self.open_help(),
            "q" | "quit" => self.should_quit = true,
            other => self.toast(format!("unknown :{other}  try :ls  :devices  :codex"), true),
        }
    }

    fn move_sel(&mut self, delta: i32) {
        let n = self.browse_len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, n - 1) as usize;
        self.list_state.select(Some(next));
    }
}

pub fn run() -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("need a terminal — use airctx ls / get / set for scripts");
    }
    let cfg = config::load().ok().flatten();
    let mut terminal = ratatui::init();
    let result = App::new(cfg).event_loop(&mut terminal);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use airctx_core::store::unix_now;
    use ratatui::backend::TestBackend;

    fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
        let mut s = String::new();
        for y in 0..buf.area.height {
            let mut x = 0;
            while x < buf.area.width {
                let sym = buf[(x, y)].symbol();
                s.push_str(sym);
                x += unicode_width::UnicodeWidthStr::width(sym).max(1) as u16;
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn login_screen_renders() {
        let mut app = App::new(None);
        app.login_url = "127.0.0.1:8787".into();
        let backend = TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| view::draw(&mut app, f)).unwrap();
        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("airctx"), "{text}");
        assert!(text.contains("url"), "{text}");
        assert!(text.contains("127.0.0.1:8787"), "{text}");
        assert!(text.contains("url"), "{text}");
        assert!(text.contains("token"), "{text}");
    }

    #[test]
    fn default_screen_is_devices_not_session_list() {
        let mut app = App::new(None);
        app.screen = Screen::Devices;
        app.cfg = Some(Config {
            server_url: "http://127.0.0.1:8787".into(),
            token: "x".into(),
            daemon: Default::default(),
        });
        app.items = vec![
            CtxItem {
                key: "devices/mac/codex/abc-123.json".into(),
                version: 2,
                size: 4000,
                updated_at: unix_now() - 30,
                expires_at: None,
            },
        ];
        app.list_state.select(Some(0));
        let backend = TestBackend::new(100, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| view::draw(&mut app, f)).unwrap();
        let text = buffer_to_string(terminal.backend().buffer());
        assert!(text.contains("devices"), "{text}");
        assert!(text.contains("mac"), "{text}");
        assert!(!text.contains("abc-123"), "session list must stay hidden on devices page: {text}");
        assert!(text.contains(":ls") || text.contains("command"), "{text}");
    }

    #[test]
    fn device_then_client_then_contexts() {
        let mut app = App::new(None);
        app.items = vec![
            CtxItem { key: "devices/mac/codex/s.json".into(), version: 1, size: 1, updated_at: 3, expires_at: None },
            CtxItem { key: "devices/pi/codex/t.json".into(), version: 1, size: 1, updated_at: 2, expires_at: None },
        ];
        let devices = app.device_rows();
        assert!(devices.iter().any(|d| d.name == "mac"));
        assert!(devices.iter().any(|d| d.name == "pi"));

        app.nav_device = Some("mac".into());
        let clients = app.client_rows();
        assert_eq!(clients[0].id, "codex");
        assert_eq!(clients[0].count, 1);
        assert!(clients.iter().any(|c| c.id == "claude-code"));
        assert!(clients.iter().any(|c| c.id == "opencode"));

        app.nav_client = Some("codex".into());
        app.screen = Screen::Contexts;
        let vis = app.visible_items();
        assert_eq!(vis.len(), 1);
        assert!(vis[0].key.contains("mac/codex"));
    }

    #[test]
    fn ls_command_opens_hidden_list() {
        let mut app = App::new(None);
        app.screen = Screen::Devices;
        app.items = vec![
            CtxItem { key: "devices/mac/codex/s.json".into(), version: 1, size: 1, updated_at: 3, expires_at: None },
        ];
        app.run_command("ls");
        assert_eq!(app.screen, Screen::Contexts);
        app.run_command("devices");
        assert_eq!(app.screen, Screen::Devices);
        assert!(app.nav_device.is_none());
    }

    #[test]
    fn session_timeline_has_user_line() {
        let v = airctx_client::sessions::SessionView {
            title: "hello".into(),
            events: vec![airctx_client::sessions::Ev {
                kind: airctx_client::sessions::EvKind::User,
                tool: String::new(),
                text: "how to deploy".into(),
                ts: String::new(),
            }],
            ..Default::default()
        };
        let d = fmt::detail_from_session("k", &v);
        assert!(d.body.iter().any(|l| matches!(l, format::LineKind::User(_))));
        assert!(d.meta.contains("hello"));
    }

}
