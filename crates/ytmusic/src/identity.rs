//! Anonymous visitor-data identity and rotation.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::context::home;
use crate::error::{Error, Result};

/// Google's consent cookie sent with anonymous requests.
pub const CONSENT_COOKIE: &str = "SOCS=CAISAiAD";

#[derive(Default)]
struct Pool {
    ids: Vec<String>,
    banned: HashSet<String>,
}

static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();

fn pool() -> &'static Mutex<Pool> {
    POOL.get_or_init(|| Mutex::new(Pool::default()))
}

/// Lock the pool, recovering the guard if a previous holder panicked.
///
/// The pool is a plain `Vec`/`HashSet` with no invariant a panic can break, so
/// recovering is strictly better than the `.unwrap()` this replaces: a panic
/// elsewhere in the process would otherwise poison the mutex and turn every
/// later metadata call into a panic too.
fn lock_pool() -> MutexGuard<'static, Pool> {
    pool()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Get a usable visitor-data id, fetching a fresh one if none are available.
pub async fn visitor_data(http: &reqwest::Client, user_agent: &str) -> Result<String> {
    if let Some(id) = reusable_id() {
        return Ok(id);
    }
    let id = fetch(http, user_agent).await?;
    lock_pool().ids.push(id.clone());
    Ok(id)
}

/// The id the pool would hand out right now, if any. Split out so the
/// selection rule is testable without a network round trip.
fn reusable_id() -> Option<String> {
    let p = lock_pool();
    p.ids.iter().find(|id| !p.banned.contains(*id)).cloned()
}

/// Drop a visitor-data id after YouTube rejects a request carrying it.
///
/// The id is removed from `ids`, not merely added to `banned`. Keeping it in
/// `ids` would make the retry in `client::post` re-select the id it just
/// banned, fail identically, and give up — so one 403 would disable search,
/// artist, album, playlist import and credits for the life of the process.
/// Evicting it means the retry's `visitor_data` call finds no candidate and
/// fetches a genuinely fresh id.
pub fn ban(id: &str) {
    let mut p = lock_pool();
    p.banned.insert(id.to_owned());
    p.ids.retain(|i| i != id);
}

async fn fetch(http: &reqwest::Client, user_agent: &str) -> Result<String> {
    let resp = http
        .get(home())
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(reqwest::header::COOKIE, CONSENT_COOKIE)
        .send()
        .await?;

    if let Some(v) = yec_from_cookie(&resp) {
        return Ok(v);
    }
    let body = resp.text().await?;
    visitor_data_from_html(&body)
        .ok_or_else(|| Error::Bootstrap("home page did not expose visitor data".into()))
}

/// The `__Secure-YEC` consent-session cookie YouTube sets on the home page.
fn yec_from_cookie(resp: &reqwest::Response) -> Option<String> {
    for cookie in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = cookie.to_str() {
            for part in s.split(';') {
                if let Some(v) = part.trim().strip_prefix("__Secure-YEC=") {
                    return Some(v.to_owned());
                }
            }
        }
    }
    None
}

/// The `"visitorData":"…"` literal in the home page's embedded `ytcfg` JSON.
///
/// Compiled once per process rather than once per call: this is on the
/// visitor-data path, which runs on the request path, and rebuilding the same
/// finite automaton for every fetch is pure waste. The pattern is a literal, so
/// compilation cannot fail — `expect` states that instead of threading an
/// `Option` through a helper whose only real failure mode is "not present".
fn visitor_data_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#""visitorData":"([\w\-%]+?)""#)
            .expect("the visitor-data pattern is a literal and cannot fail to compile")
    })
}

fn visitor_data_from_html(html: &str) -> Option<String> {
    visitor_data_regex()
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
}

/// Replace the pool with exactly `ids`, in order, and clear every ban.
///
/// Replaces rather than appends: a test that needs a *specific* visitor on the
/// first request can only get that if it is the sole candidate, and the pool
/// outlives individual tests. Clearing the bans means a ban left behind by
/// another test cannot make this one fail for the wrong reason.
///
/// This fixes the pool's *contents*; it does not stop another module's test from
/// using them mid-test. Every caller must hold [`pool_gate`] for the whole test.
#[cfg(test)]
pub(crate) fn seed_pool_for_test(ids: &[&str]) {
    let mut p = lock_pool();
    p.ids = ids.iter().map(|id| (*id).to_owned()).collect();
    p.banned.clear();
}

/// The one gate every test that touches the process-global visitor pool holds.
///
/// The pool models a process-wide cache, so it is deliberately not per-test —
/// and two modules use it: `identity::tests` reads and bans, `client::tests`
/// seeds it and then checks which id went out. A gate per module does not
/// exclude the other one. Once a client test has seeded
/// `visitor-for-context-test`, an identity test that assumed an empty pool fails
/// on `reusable_id()`'s first-non-banned lookup, and whether it does depends on
/// thread scheduling rather than on the code under test: green under default
/// parallelism, red under `--test-threads=1`. That is the same "green for the
/// wrong reason" shape as the L17 harness bugs, so there is one gate, not two.
///
/// A `tokio` mutex rather than a `std` one because the client tests hold it
/// across `.await` points. The synchronous identity tests take it with
/// `blocking_lock`, which is safe here: libtest gives each test its own thread,
/// and none of these enters a runtime context.
#[cfg(test)]
pub(crate) fn pool_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H1's regression guard: after `ban`, the banned id must not come back.
    ///
    /// Before the fix, `ban` only inserted into `banned` and left the id in
    /// `ids`, so `client::post`'s retry re-selected the id it had just banned
    /// and failed identically — one 403 then disabled search, artist, album,
    /// playlist import and credits for the life of the process. Selection is
    /// isolated in `reusable_id`, so this needs no network at all.
    ///
    /// The gate and the explicit seed are both load-bearing: the pool is shared
    /// with `client::tests`, so neither "no other test is running" nor "the pool
    /// is empty" is this test's to assume.
    #[test]
    fn a_banned_id_is_never_handed_back() {
        let _gate = pool_gate().blocking_lock();
        const SEEDED: &str = "test-identity-ban-seeded-id";
        seed_pool_for_test(&[SEEDED]);

        // While usable, the seeded id is what the pool hands out.
        assert_eq!(reusable_id().as_deref(), Some(SEEDED));

        ban(SEEDED);
        assert!(
            !lock_pool().ids.iter().any(|i| i == SEEDED),
            "ban must evict the id from the pool, not just mark it"
        );
        assert!(lock_pool().banned.contains(SEEDED));
        assert_eq!(
            reusable_id(),
            None,
            "the retry must not re-select the id it just banned"
        );
    }

    /// A banned id must not shadow a usable one behind it.
    #[test]
    fn a_banned_id_does_not_hide_the_others() {
        let _gate = pool_gate().blocking_lock();
        const BAD: &str = "test-identity-ban-bad";
        const GOOD: &str = "test-identity-ban-good";
        seed_pool_for_test(&[BAD, GOOD]);
        ban(BAD);
        assert_eq!(reusable_id().as_deref(), Some(GOOD));
    }

    /// L12: a panic while the pool is locked must not turn every later call into
    /// a panic.
    ///
    /// The guard held across the panic poisons the mutex, and `lock_pool` is the
    /// only reason that is recoverable: under the `.unwrap()` this replaced, one
    /// unrelated panic anywhere in the process would make every metadata request
    /// panic too, for the life of the process. The poisoning is deliberate and
    /// permanent for this test binary — every accessor goes through `lock_pool`,
    /// so the rest of the suite then runs on the recovery path rather than
    /// around it.
    #[test]
    fn a_poisoned_pool_is_recovered_rather_than_repainted_as_a_panic() {
        let _gate = pool_gate().blocking_lock();
        const SEEDED: &str = "test-identity-poison-recovery";
        seed_pool_for_test(&[SEEDED]);

        // Poison the process-wide mutex exactly the way a panic elsewhere would.
        let panicked = std::panic::catch_unwind(|| {
            let _held = pool().lock().expect("the pool starts unpoisoned");
            panic!("simulated panic while holding the visitor pool");
        });
        assert!(panicked.is_err(), "the closure must have panicked");
        assert!(
            pool().lock().is_err(),
            "the mutex must now be poisoned, or this test proves nothing"
        );

        // And the accessor every caller uses recovers instead of panicking.
        assert_eq!(reusable_id().as_deref(), Some(SEEDED));
    }

    /// L7's regression guard: the regex is compiled once, not once per request.
    ///
    /// `ptr::eq` is the observable consequence of caching — two calls through the
    /// accessor must hand back the *same* compiled object. The behavioural test
    /// below passes either way, which is precisely why this one exists: without
    /// it, dropping the `OnceLock` again would be invisible to the suite.
    #[test]
    fn the_visitor_data_regex_is_compiled_once() {
        assert!(
            std::ptr::eq(visitor_data_regex(), visitor_data_regex()),
            "the visitor-data regex must be cached, not rebuilt on every call"
        );
    }

    /// Behavioural half of L7: extraction still works through the cached regex,
    /// and still refuses shapes it should not match.
    #[test]
    fn visitor_data_is_read_from_the_home_page_json() {
        let html = r#"<script>window["ytcfg"].set({"VISITOR_DATA":"decoy","visitorData":"CgsxMjM0NQ%3D%3D","X":"y"});</script>"#;
        assert_eq!(
            visitor_data_from_html(html).as_deref(),
            Some("CgsxMjM0NQ%3D%3D"),
            "the cookie-shaped value, percent-escapes included, is the answer"
        );
        // The key is case-sensitive: `VISITOR_DATA` is a different field.
        assert_eq!(visitor_data_from_html(r#"{"VISITOR_DATA":"decoy"}"#), None);
        assert_eq!(visitor_data_from_html("<html></html>"), None);
        // The capture requires at least one character, so an empty value is
        // "absent" rather than an empty id the client would send.
        assert_eq!(visitor_data_from_html(r#"{"visitorData":""}"#), None);
    }
}
