//! 风控第一层:固定窗口限流 + 认证失败封禁。
//! 全部内存态,无额外分配放大;状态量按 IP 隔离。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_REQUESTS_PER_WINDOW: u32 = 1200;
const MAX_AUTH_FAILURES: u32 = 10;
const BAN_DURATION: Duration = Duration::from_secs(10 * 60);
/// 状态表超过该规模时清理过期条目,防内存被伪造来源 IP 撑大
const CLEANUP_THRESHOLD: usize = 4096;

struct IpState {
    window_start: Instant,
    requests: u32,
    auth_failures: u32,
    banned_until: Option<Instant>,
}

pub struct RateLimiter {
    inner: RwLock<HashMap<IpAddr, IpState>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self { inner: RwLock::new(HashMap::new()) }
    }

    /// 请求是否放行(含封禁与速率检查)
    pub fn check_request(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.inner.write().unwrap();
        if map.len() > CLEANUP_THRESHOLD {
            map.retain(|_, st| match st.banned_until {
                Some(until) if now < until => true,
                _ => now.duration_since(st.window_start) < WINDOW,
            });
        }
        let st = map.entry(ip).or_insert_with(|| IpState {
            window_start: now,
            requests: 0,
            auth_failures: 0,
            banned_until: None,
        });
        if let Some(until) = st.banned_until {
            if now < until {
                return false;
            }
            st.banned_until = None;
            st.auth_failures = 0;
        }
        if now.duration_since(st.window_start) >= WINDOW {
            st.window_start = now;
            st.requests = 0;
        }
        st.requests += 1;
        st.requests <= MAX_REQUESTS_PER_WINDOW
    }

    pub fn report_auth_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut map = self.inner.write().unwrap();
        let st = map.entry(ip).or_insert_with(|| IpState {
            window_start: now,
            requests: 0,
            auth_failures: 0,
            banned_until: None,
        });
        if now.duration_since(st.window_start) >= WINDOW {
            st.window_start = now;
            st.auth_failures = 0;
        }
        st.auth_failures += 1;
        if st.auth_failures >= MAX_AUTH_FAILURES {
            st.banned_until = Some(now + BAN_DURATION);
            st.auth_failures = 0;
            tracing::warn!(%ip, ban_secs = BAN_DURATION.as_secs(), "auth failures exceeded, banning source IP");
        }
    }
}
