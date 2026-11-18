use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TOKEN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_TOKENS: usize = 32;
const FAIL_WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILS_PER_WINDOW: u32 = 15;

struct AuthState {
    tokens: HashMap<String, Instant>,
    fail_window_start: Instant,
    fail_count: u32,
}

pub struct TokenStore {
    state: Mutex<AuthState>,
}

impl TokenStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(AuthState {
                tokens: HashMap::new(),
                fail_window_start: Instant::now(),
                fail_count: 0,
            }),
        }
    }

    /// 检查是否处于登录失败限流中
    pub fn is_rate_limited(&self) -> bool {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if now.duration_since(st.fail_window_start) >= FAIL_WINDOW {
            st.fail_window_start = now;
            st.fail_count = 0;
            return false;
        }
        st.fail_count >= MAX_FAILS_PER_WINDOW
    }

    pub fn record_failure(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if now.duration_since(st.fail_window_start) >= FAIL_WINDOW {
            st.fail_window_start = now;
            st.fail_count = 1;
        } else {
            st.fail_count = st.fail_count.saturating_add(1);
        }
    }

    pub fn issue(&self) -> String {
        let mut raw = [0u8; 16];
        if getrandom::getrandom(&mut raw).is_err() {
            // 极端环境下兜底（几乎不可能发生）
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            raw = nanos.to_le_bytes();
        }
        let token = hex::encode(raw);

        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        st.tokens.retain(|_, exp| *exp > now);

        // 限制最大活跃 token 数，防止低内存路由器被恶意登录耗尽内存
        while st.tokens.len() >= MAX_TOKENS {
            let Some(oldest_key) = st
                .tokens
                .iter()
                .min_by_key(|(_, exp)| *exp)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            st.tokens.remove(&oldest_key);
        }

        st.tokens.insert(token.clone(), now + TOKEN_TTL);
        token
    }

    pub fn check(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        st.tokens.retain(|_, exp| *exp > now);
        st.tokens.contains_key(token)
    }

    pub fn revoke(&self, token: &str) {
        if token.is_empty() {
            return;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.tokens.remove(token);
    }

    /// 密码或用户名修改后，吊销除当前会话外的所有 token
    pub fn revoke_others(&self, keep_token: &str) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.tokens.retain(|k, _| k == keep_token);
    }
}

/// 常量时间字节比较，避免时序侧信道
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_lifecycle_and_cap() {
        let store = TokenStore::new();
        let t1 = store.issue();
        assert_eq!(t1.len(), 32);
        assert!(store.check(&t1));
        store.revoke(&t1);
        assert!(!store.check(&t1));

        for _ in 0..40 {
            store.issue();
        }
        let st = store.state.lock().unwrap();
        assert!(st.tokens.len() <= MAX_TOKENS);
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"admin", b"admin"));
        assert!(!constant_time_eq(b"admin", b"admiN"));
        assert!(!constant_time_eq(b"admin", b"admin1"));
    }
}
