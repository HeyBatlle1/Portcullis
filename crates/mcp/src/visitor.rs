//! Who is asking, roughly, and how many times today.
//!
//! The public pages can start things that cost model calls: a console run, a
//! siege. Each already has a site-wide daily cap, which is what bounds the
//! bill. This is the other half: one visitor should not be able to spend the
//! whole day's cap in ten minutes and leave nothing for the next person.
//!
//! It is a speed bump, not a wall. An address is a poor name for a person:
//! a phone hops networks, an office shares one, and someone determined can
//! rotate through many. The site-wide cap is what holds when this does not.

use axum::http::HeaderMap;
use portcullis_kernel::Timestamp;
use std::collections::HashMap;
use std::sync::Mutex;

/// The visitor's address as the edge saw it.
///
/// On Render, Cloudflare sits in front and sets `CF-Connecting-IP` itself,
/// overwriting anything the client sent. Two others are deliberately not
/// read: `X-Forwarded-For`, because Render appends to it rather than
/// replacing it, so its first entry is whatever the client chose; and
/// `True-Client-IP`, which Cloudflare only sets when a zone turns it on, and
/// otherwise passes through from the client untouched.
///
/// Without it (running locally, or if the hosting ever changes) there is no
/// visitor to count, and only the site-wide cap applies. The other choice,
/// one shared bucket for everyone, would quietly turn the public demo off
/// after a few uses the day the header went missing.
///
/// An IPv6 address is counted by its /64: one machine is routinely given a
/// whole /64, so counting single addresses would hand it billions of visitors.
pub fn client(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)?;
    Some(match raw.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(a)) => {
            let s = a.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        _ => raw.to_string(),
    })
}

/// The judges' pass. Judges are given a link, `/judge?key=...`, in the
/// private testing instructions; it sets a cookie, and a request carrying it
/// skips the per-visitor allowance and the public daily caps, so a stranger
/// who used up today's runs cannot lock the people judging out. The cookie
/// is `SameSite=Strict`, so another site cannot make a browser send it.
/// Off unless `PORTCULLIS_JUDGE_KEY` is set.
static JUDGE_TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub const JUDGE_COOKIE: &str = "portcullis_judge";

/// The visitor name a judge's request carries into the handlers. It has a
/// space in it, so no address the edge reports can ever equal it.
pub const JUDGE: &str = "judge pass";

/// Who is asking: a judge, an address, or nobody the edge named.
pub fn who(headers: &HeaderMap) -> Option<String> {
    if is_judge(headers) {
        Some(JUDGE.to_string())
    } else {
        client(headers)
    }
}

/// What the cookie holds for a key: its SHA-256, so the key is never echoed.
pub fn judge_token(key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(format!("portcullis-judge:{key}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Set once at startup. A key shorter than 16 characters is refused.
pub fn set_judge_key(key: &str) -> Result<(), String> {
    if key.len() < 16 {
        return Err("PORTCULLIS_JUDGE_KEY must be at least 16 characters".into());
    }
    let _ = JUDGE_TOKEN.set(judge_token(key));
    Ok(())
}

/// Whether this key is the judges' key.
pub fn judge_key_matches(key: &str) -> bool {
    JUDGE_TOKEN.get().is_some_and(|t| *t == judge_token(key))
}

/// Whether this request carries the judges' cookie.
pub fn is_judge(headers: &HeaderMap) -> bool {
    let Some(want) = JUDGE_TOKEN.get() else {
        return false;
    };
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .any(|(k, v)| k == JUDGE_COOKIE && v == want)
}

/// So many per visitor per UTC day.
pub struct Quota {
    per_day: u32,
    used: Mutex<(u64, HashMap<String, u32>)>,
}

impl Quota {
    pub fn new(per_day: u32) -> Quota {
        Quota {
            per_day,
            used: Mutex::new((0, HashMap::new())),
        }
    }

    pub fn per_day(&self) -> u32 {
        self.per_day
    }

    /// Spend one of `who`'s for today, or say no without spending anything.
    /// An unnamed visitor is not counted; see [`client`].
    pub fn take(&self, who: Option<&str>, now: Timestamp) -> bool {
        let Some(who) = who else { return true };
        let mut g = self.used.lock().unwrap_or_else(|p| p.into_inner());
        let day = now / 86_400;
        if g.0 != day {
            // Yesterday's counts are dropped whole, which also bounds the map.
            *g = (day, HashMap::new());
        }
        let n = g.1.entry(who.to_string()).or_default();
        if *n >= self.per_day {
            return false;
        }
        *n += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_visitor_gets_their_own_allowance_and_it_resets_daily() {
        let q = Quota::new(2);
        let t = 10 * 86_400 + 5;
        assert!(q.take(Some("a"), t));
        assert!(q.take(Some("a"), t));
        assert!(!q.take(Some("a"), t), "third of two");
        assert!(q.take(Some("b"), t), "someone else is unaffected");
        assert!(q.take(Some("a"), t + 86_400), "a new day");
        assert!((0..10).all(|_| q.take(None, t)), "nobody to count");
    }

    #[test]
    fn only_the_edge_header_names_the_visitor() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "1.2.3.4".parse().unwrap());
        h.insert("true-client-ip", "5.6.7.8".parse().unwrap());
        assert_eq!(client(&h), None, "client-settable headers ignored");
        h.insert("cf-connecting-ip", "203.0.113.9".parse().unwrap());
        assert_eq!(client(&h).as_deref(), Some("203.0.113.9"));
    }

    #[test]
    fn an_ipv6_visitor_is_their_whole_slash_64() {
        let mut a = HeaderMap::new();
        a.insert("cf-connecting-ip", "2001:db8:1:2::1".parse().unwrap());
        let mut b = HeaderMap::new();
        b.insert("cf-connecting-ip", "2001:db8:1:2:ffff::9".parse().unwrap());
        let mut c = HeaderMap::new();
        c.insert("cf-connecting-ip", "2001:db8:1:3::1".parse().unwrap());
        assert_eq!(client(&a), client(&b));
        assert_ne!(client(&a), client(&c));
    }
}
