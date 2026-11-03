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
pub fn client(headers: &HeaderMap) -> Option<String> {
    headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string)
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
}
