//! A small limit on sign-in starts per network address, so nobody can flood
//! the database with pending sign-ins. Only `/api/auth/login` is limited: the
//! callback needs a one-time state that only a login can create, so it is
//! bounded by this limit too, and a limited callback would waste a real sign-in.
//!
//! Fixed window, in memory. Behind Docker Desktop every request can arrive from
//! the same gateway address, so the limit may act as one shared count. The
//! default is generous for a small team either way.

use std::{
    collections::HashMap,
    hash::Hash,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};

use crate::app::AppState;

/// Sign-in starts allowed per address per minute.
pub const SIGN_IN_PER_MINUTE: u32 = 30;
/// Most addresses tracked at once. When full, old windows are pruned; if it is
/// still full, new addresses are let through untracked rather than locked out.
const MAX_TRACKED: usize = 10_000;

/// Counts attempts per key (a network address, or a user id).
pub struct RateLimiter<K = IpAddr> {
    max: u32,
    window: Duration,
    hits: Mutex<HashMap<K, (Instant, u32)>>,
}

impl<K: Eq + Hash> RateLimiter<K> {
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Count one attempt for `key`. False once the limit for this window is used.
    pub fn allow(&self, key: K) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        if hits.len() >= MAX_TRACKED && !hits.contains_key(&key) {
            hits.retain(|_, (start, _)| now.duration_since(*start) < self.window);
            if hits.len() >= MAX_TRACKED {
                return true;
            }
        }
        let entry = hits.entry(key).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        entry.1 <= self.max
    }
}

/// Middleware for the sign-in start. Once the limit is reached the browser is
/// sent back to the sign-in screen, which explains the wait.
pub async fn sign_in(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if !state.sign_in_limit.allow(ip) {
        tracing::warn!(%ip, "too many sign-in attempts");
        return Redirect::to("/?signin=busy").into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_limit_then_refuses() {
        let l = RateLimiter::new(3, Duration::from_secs(60));
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(l.allow(a) && l.allow(a) && l.allow(a));
        assert!(!l.allow(a));
        assert!(l.allow(b), "each address has its own count");
    }

    #[test]
    fn a_full_table_lets_new_addresses_through() {
        let l = RateLimiter::new(1, Duration::from_secs(60));
        for i in 0..MAX_TRACKED as u32 {
            assert!(l.allow(IpAddr::from(i.to_be_bytes())));
        }
        let new: IpAddr = "250.0.0.1".parse().unwrap();
        assert!(l.allow(new) && l.allow(new), "untracked, never locked out");
        assert!(
            !l.allow(IpAddr::from(0u32.to_be_bytes())),
            "tracked ones still count"
        );
    }

    #[test]
    fn a_new_window_starts_fresh() {
        let l = RateLimiter::new(1, Duration::from_millis(20));
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(l.allow(a));
        assert!(!l.allow(a));
        std::thread::sleep(Duration::from_millis(30));
        assert!(l.allow(a));
    }
}
