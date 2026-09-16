//! SQLite 存储层。WAL 持久化;token 只存 SHA-256(常数时间比较);写操作追加 audit_log。

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::event::Event;

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(raw.as_bytes()))
}

/// 常数时间相等比较,防时序侧信道
fn ct_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn random_hex(n: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// LIKE 前缀匹配需要转义 %、_、\
fn like_escape(prefix: &str) -> String {
    prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

#[derive(Debug)]
pub struct TokenInfo {
    pub id: i64,
    pub name: String,
}

pub struct Entry {
    pub value: Vec<u8>,
    pub content_type: String,
    pub version: i64,
    pub updated_at: i64,
    expires_at: Option<i64>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ListItem {
    pub key: String,
    pub version: i64,
    pub size: usize,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct PutResult {
    pub key: String,
    pub version: i64,
    pub updated_at: i64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -20000;
             PRAGMA mmap_size = 268435456;
             PRAGMA temp_store = MEMORY;",
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                k TEXT PRIMARY KEY,
                v TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS tokens (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                last_used_at INTEGER,
                revoked INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS entries (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL,
                content_type TEXT NOT NULL DEFAULT 'application/octet-stream',
                version INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                expires_at INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_entries_version ON entries(version);
             CREATE INDEX IF NOT EXISTS idx_entries_expires ON entries(expires_at) WHERE expires_at IS NOT NULL;
             CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                token_id INTEGER,
                action TEXT NOT NULL,
                key TEXT,
                ip TEXT,
                ua TEXT
             );
             CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_salt TEXT NOT NULL,
                password_hash TEXT NOT NULL,
                is_admin INTEGER NOT NULL DEFAULT 0,
                api_token TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS invites (
                code TEXT PRIMARY KEY,
                created_by INTEGER NOT NULL,
                used_by INTEGER,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS web_sessions (
                token_hash TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
             );",
        )?;
        Ok(())
    }

    fn validate_key(key: &str) -> Result<()> {
        if key.is_empty() {
            return Err(Error::InvalidRequest("key must not be empty".into()));
        }
        if key.len() > 512 {
            return Err(Error::InvalidRequest("key too long (max 512 bytes)".into()));
        }
        if key.contains('\0') {
            return Err(Error::InvalidRequest("key contains illegal characters".into()));
        }
        Ok(())
    }

    /// 事务内递增全局版本号
    fn bump_version(tx: &rusqlite::Transaction<'_>) -> Result<i64> {
        tx.execute("INSERT OR IGNORE INTO meta(k, v) VALUES('version', 0)", [])?;
        let v: i64 = tx.query_row(
            "SELECT CAST(v AS INTEGER) FROM meta WHERE k='version'",
            [],
            |r| r.get(0),
        )?;
        tx.execute("UPDATE meta SET v = ?1 WHERE k='version'", params![v + 1])?;
        Ok(v + 1)
    }

    pub fn put(
        &mut self,
        key: &str,
        value: &[u8],
        content_type: &str,
        ttl_secs: Option<i64>,
        now: i64,
    ) -> Result<PutResult> {
        Self::validate_key(key)?;
        if content_type.len() > 128 {
            return Err(Error::InvalidRequest("content-type too long".into()));
        }
        if let Some(ttl) = ttl_secs {
            if ttl < 0 {
                return Err(Error::InvalidRequest("ttl must be non-negative seconds".into()));
            }
        }
        let expires_at = ttl_secs.map(|t| now + t);

        let tx = self.conn.transaction()?;
        let version = Self::bump_version(&tx)?;
        tx.execute(
            "INSERT INTO entries(key, value, content_type, version, updated_at, expires_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(key) DO UPDATE SET
               value = excluded.value,
               content_type = excluded.content_type,
               version = excluded.version,
               updated_at = excluded.updated_at,
               expires_at = excluded.expires_at",
            params![key, value, content_type, version, now, expires_at],
        )?;
        tx.commit()?;
        Ok(PutResult { key: key.to_string(), version, updated_at: now })
    }

    /// 读取;已过期的条目惰性删除后返回 None
    pub fn get(&mut self, key: &str, now: i64) -> Result<Option<Entry>> {
        let row = self
            .conn
            .query_row(
                "SELECT value, content_type, version, updated_at, expires_at FROM entries WHERE key = ?1",
                params![key],
                |r| {
                    Ok(Entry {
                        value: r.get(0)?,
                        content_type: r.get(1)?,
                        version: r.get(2)?,
                        updated_at: r.get(3)?,
                        expires_at: r.get(4)?,
                    })
                },
            )
            .optional()?;
        match row {
            Some(e) if e.expires_at.map(|t| t <= now).unwrap_or(false) => {
                self.delete(key, now)?;
                Ok(None)
            }
            other => Ok(other),
        }
    }

    /// 删除;返回删除事件的版本号(None = key 不存在)
    pub fn delete(&mut self, key: &str, now: i64) -> Result<Option<i64>> {
        let tx = self.conn.transaction()?;
        let existed: Option<i64> = tx
            .query_row("SELECT version FROM entries WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?;
        let Some(_) = existed else {
            tx.commit()?;
            return Ok(None);
        };
        let version = Self::bump_version(&tx)?;
        tx.execute("DELETE FROM entries WHERE key = ?1", params![key])?;
        tx.commit()?;
        let _ = now;
        Ok(Some(version))
    }

    pub fn list(&mut self, prefix: &str, limit: u32, now: i64) -> Result<Vec<ListItem>> {
        let pat = format!("{}%", like_escape(prefix));
        let mut stmt = self.conn.prepare(
            "SELECT key, version, length(value), updated_at, expires_at FROM entries
             WHERE (expires_at IS NULL OR expires_at > ?1) AND key LIKE ?2 ESCAPE '\\'
             ORDER BY key LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![now, pat, limit], |r| {
            Ok(ListItem {
                key: r.get(0)?,
                version: r.get(1)?,
                size: r.get::<_, i64>(2)? as usize,
                updated_at: r.get(3)?,
                expires_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 回放 version > since 的当前存活条目(watch 重连补差量;中间已删除的状态
    /// 不可重现,客户端以当前状态为准做最终一致对账)
    pub fn changed_since(&mut self, since: i64, prefix: &str, limit: u32, now: i64) -> Result<Vec<Event>> {
        let pat = format!("{}%", like_escape(prefix));
        let mut stmt = self.conn.prepare(
            "SELECT key, version, updated_at FROM entries
             WHERE version > ?1 AND (expires_at IS NULL OR expires_at > ?2) AND key LIKE ?3 ESCAPE '\\'
             ORDER BY version ASC LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![since, now, pat, limit], |r| {
            Ok(Event::Set { key: r.get(0)?, version: r.get(1)?, updated_at: r.get(2)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 清理过期条目,返回 (key, 删除事件版本) 列表
    pub fn purge_expired(&mut self, now: i64) -> Result<Vec<(String, i64)>> {
        let tx = self.conn.transaction()?;
        let keys: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT key FROM entries WHERE expires_at IS NOT NULL AND expires_at <= ?1 LIMIT 500",
            )?;
            let rows = stmt.query_map(params![now], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        if keys.is_empty() {
            tx.commit()?;
            return Ok(vec![]);
        }
        tx.execute("INSERT OR IGNORE INTO meta(k, v) VALUES('version', 0)", [])?;
        let mut cur: i64 = tx.query_row(
            "SELECT CAST(v AS INTEGER) FROM meta WHERE k='version'",
            [],
            |r| r.get(0),
        )?;
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            tx.execute("DELETE FROM entries WHERE key = ?1", params![k])?;
            cur += 1;
            out.push((k, cur));
        }
        tx.execute("UPDATE meta SET v = ?1 WHERE k='version'", params![cur])?;
        tx.commit()?;
        Ok(out)
    }

    /// 首次启动创建管理 token,明文只返回这一次
    pub fn init_admin_token(&mut self, name: &str, now: i64) -> Result<Option<String>> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM tokens", [], |r| r.get(0))?;
        if n > 0 {
            return Ok(None);
        }
        let raw = format!("ctx1_{}", random_hex(32));
        self.conn.execute(
            "INSERT INTO tokens(name, token_hash, created_at) VALUES(?1, ?2, ?3)",
            params![name, hash_token(&raw), now],
        )?;
        Ok(Some(raw))
    }

    pub fn verify_token(&mut self, raw: &str) -> Option<TokenInfo> {
        let hash = hash_token(raw);
        let Ok(mut stmt) = self.conn.prepare("SELECT id, name, token_hash FROM tokens WHERE revoked = 0")
        else {
            return None;
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        }) else {
            return None;
        };
        for (id, name, h) in rows.flatten() {
            if ct_eq(&h, &hash) {
                return Some(TokenInfo { id, name });
            }
        }
        None
    }

    pub fn audit(&mut self, token_id: i64, action: &str, key: &str, ip: &str, ua: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO audit_log(ts, token_id, action, key, ip, ua) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![now, token_id, action, key, ip, ua],
        )?;
        Ok(())
    }

    pub fn user_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
    }

    pub fn get_user_by_name(&self, username: &str) -> Result<Option<User>> {
        let row = self.conn.query_row(
            "SELECT id, username, password_salt, password_hash, is_admin, api_token, created_at FROM users WHERE username = ?1",
            params![username],
            |r| {
                Ok(User {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    password_salt: r.get(2)?,
                    password_hash: r.get(3)?,
                    is_admin: r.get::<_, i64>(4)? != 0,
                    api_token: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        );
        match row {
            Ok(u) => Ok(Some(u)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_user(&self, id: i64) -> Result<Option<User>> {
        let row = self.conn.query_row(
            "SELECT id, username, password_salt, password_hash, is_admin, api_token, created_at FROM users WHERE id = ?1",
            params![id],
            |r| {
                Ok(User {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    password_salt: r.get(2)?,
                    password_hash: r.get(3)?,
                    is_admin: r.get::<_, i64>(4)? != 0,
                    api_token: r.get(5)?,
                    created_at: r.get(6)?,
                })
            },
        );
        match row {
            Ok(u) => Ok(Some(u)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn register_user(&mut self, username: &str, password: &str, invite: Option<&str>, now: i64) -> Result<User> {
        let username = username.trim();
        validate_username(username)?;
        if password.len() < 6 {
            return Err(Error::InvalidRequest("password must be at least 6 characters".into()));
        }
        if self.get_user_by_name(username)?.is_some() {
            return Err(Error::InvalidRequest("username already taken".into()));
        }
        let n = self.user_count()?;
        let is_admin = n == 0;
        if n > 0 {
            let Some(code) = invite.filter(|s| !s.is_empty()) else {
                return Err(Error::InvalidRequest("registration is closed; ask an admin for an invite link".into()));
            };
            let used: Option<Option<i64>> = self
                .conn
                .query_row(
                    "SELECT used_by FROM invites WHERE code = ?1",
                    params![code],
                    |r| r.get(0),
                )
                .optional()?;
            match used {
                None => return Err(Error::InvalidRequest("invalid invite".into())),
                Some(Some(_)) => return Err(Error::InvalidRequest("invite already used".into())),
                Some(None) => {}
            }
        }

        let salt = random_hex(16);
        let ph = hash_token(&format!("{salt}:{password}"));
        let api_token = format!("ctx1_{}", random_hex(32));
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO users(username, password_salt, password_hash, is_admin, api_token, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![username, salt, ph, if is_admin { 1 } else { 0 }, api_token, now],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO tokens(name, token_hash, created_at) VALUES(?1, ?2, ?3)",
            params![username, hash_token(&api_token), now],
        )?;
        if let Some(code) = invite.filter(|s| !s.is_empty()) {
            tx.execute("UPDATE invites SET used_by = ?1 WHERE code = ?2 AND used_by IS NULL", params![id, code])?;
        }
        tx.commit()?;
        let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
        Ok(User {
            id,
            username: username.to_string(),
            password_salt: salt,
            password_hash: ph,
            is_admin,
            api_token,
            created_at: now,
        })
    }

    pub fn verify_password(&self, username: &str, password: &str) -> Result<Option<User>> {
        let Some(u) = self.get_user_by_name(username)? else {
            return Ok(None);
        };
        let expect = hash_token(&format!("{}:{password}", u.password_salt));
        if ct_eq(&expect, &u.password_hash) {
            Ok(Some(u))
        } else {
            Ok(None)
        }
    }

    pub fn create_invite(&mut self, admin_id: i64, now: i64) -> Result<String> {
        let code = random_hex(12);
        self.conn.execute(
            "INSERT INTO invites(code, created_by, created_at) VALUES(?1, ?2, ?3)",
            params![code, admin_id, now],
        )?;
        Ok(code)
    }

    pub fn create_web_session(&mut self, user_id: i64, now: i64) -> Result<String> {
        let raw = random_hex(32);
        let ttl = 30 * 24 * 3600;
        self.conn.execute(
            "INSERT INTO web_sessions(token_hash, user_id, expires_at) VALUES(?1, ?2, ?3)",
            params![hash_token(&raw), user_id, now + ttl],
        )?;
        Ok(raw)
    }

    pub fn user_from_session(&mut self, raw: &str, now: i64) -> Result<Option<User>> {
        let hash = hash_token(raw);
        let row = self.conn.query_row(
            "SELECT user_id, expires_at FROM web_sessions WHERE token_hash = ?1",
            params![hash],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        );
        let Ok((uid, exp)) = row else { return Ok(None) };
        if exp < now {
            let _ = self.conn.execute("DELETE FROM web_sessions WHERE token_hash = ?1", params![hash]);
            return Ok(None);
        }
        self.get_user(uid)
    }

    pub fn drop_web_session(&mut self, raw: &str) -> Result<()> {
        self.conn.execute("DELETE FROM web_sessions WHERE token_hash = ?1", params![hash_token(raw)])?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub password_salt: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub api_token: String,
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> Store {
        let p = std::env::temp_dir().join(format!("airctx-user-{}.db", random_hex(8)));
        Store::open(&p).unwrap()
    }

    #[test]
    fn first_user_is_admin_then_invite_only() {
        let mut s = tmp();
        let now = 1_700_000_000;
        let a = s.register_user("alice", "secret1", None, now).unwrap();
        assert!(a.is_admin);
        assert!(a.api_token.starts_with("ctx1_"));
        assert_eq!(s.user_count().unwrap(), 1);
        assert!(s.register_user("bob", "secret1", None, now).is_err());
        let code = s.create_invite(a.id, now).unwrap();
        let b = s.register_user("bob", "secret1", Some(&code), now).unwrap();
        assert!(!b.is_admin);
        assert!(s.register_user("carol", "secret1", Some(&code), now).is_err());
        assert!(s.verify_password("alice", "secret1").unwrap().is_some());
        assert!(s.verify_password("alice", "nope").unwrap().is_none());
    }
}

fn validate_username(name: &str) -> Result<()> {
    if name.len() < 3 || name.len() > 32 {
        return Err(Error::InvalidRequest("username must be 3-32 characters".into()));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(Error::InvalidRequest("username may contain letters, digits, _ and -".into()));
    }
    Ok(())
}
