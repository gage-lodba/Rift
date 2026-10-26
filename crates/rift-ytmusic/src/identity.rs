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
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// Drop a visitor-data id after `YouTube` rejects a request carrying it.
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

/// Largest home-page body the visitor scrape will buffer.
///
/// The page is a few hundred kilobytes; this is a generous ceiling that still
/// stops a broken (or hostile) response from growing the scrape buffer for as
/// long as it keeps sending. The request's own 30 s timeout bounds time, not
/// bytes.
const HOME_MAX_BYTES: usize = 2 * 1024 * 1024;

async fn fetch(http: &reqwest::Client, user_agent: &str) -> Result<String> {
    let resp = http
        .get(home())
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(reqwest::header::COOKIE, CONSENT_COOKIE)
        .send()
        .await?
        // A non-success page cannot carry a usable `visitorData`; report the
        // status instead of the generic "home page did not expose visitor data".
        .error_for_status()?;

    // The cookie is read before the body because it is the cheaper source, but
    // it is not enough on its own: `YouTube` currently sets `__Secure-YEC=` with
    // no value (observed 2026-10-26). An empty visitor id is not "anonymous" —
    // the player and metadata endpoints answer `LOGIN_REQUIRED`/"Sign in to
    // confirm you're not a bot" for it — so an empty cookie has to fall through
    // to the `visitorData` literal embedded in the page.
    let yec = yec_from_cookie(&resp);
    let body = read_home_body(resp).await?;
    resolve_visitor(yec, &body)
        .ok_or_else(|| Error::Bootstrap("home page did not expose visitor data".into()))
}

/// Read the home page with a hard size ceiling (see [`HOME_MAX_BYTES`]).
async fn read_home_body(mut resp: reqwest::Response) -> Result<String> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > HOME_MAX_BYTES {
            return Err(Error::Bootstrap(
                "home page body is too large to scrape visitor data".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Choose the visitor id from one home-page response.
///
/// A non-empty `__Secure-YEC` cookie wins: it is the id the page was served
/// with. An empty one (or no cookie at all) falls through to the
/// `"visitorData"` literal embedded in the page's `ytcfg` JSON. Both halves are
/// pinned by `an_empty_yec_cookie_falls_through_to_the_page_visitor_data`, so the
/// fallback cannot silently take over the happy path.
fn resolve_visitor(yec: Option<String>, body: &str) -> Option<String> {
    yec.filter(|id| !id.is_empty())
        .or_else(|| visitor_data_from_html(body))
}

/// The `__Secure-YEC` consent-session cookie `YouTube` sets on the home page.
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
/// compilation cannot fail in practice, but the helper still returns `Option`
/// rather than panicking: if it ever did fail (`None`), the caller reports the
/// existing "home page did not expose visitor data" error instead of aborting.
/// The `OnceLock` keeps the fast path (no per-call recompilation).
fn visitor_data_regex() -> Option<&'static regex::Regex> {
    static RE: OnceLock<Option<regex::Regex>> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#""visitorData":"([\w\-%]+?)""#).ok())
        .as_ref()
}

fn visitor_data_from_html(html: &str) -> Option<String> {
    visitor_data_regex()?
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
        let (first, second) = (visitor_data_regex(), visitor_data_regex());
        assert!(
            matches!((first, second), (Some(a), Some(b)) if std::ptr::eq(a, b)),
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

    /// The source rule for a home-page visitor id: a non-empty `__Secure-YEC`
    /// cookie wins; an empty one falls through to the page's `visitorData`.
    ///
    /// Live regression guard (observed 2026-10-26): `YouTube` sets
    /// `Set-Cookie: __Secure-YEC=; …` with no value. Before the fallback, the
    /// pool was filled with `""` and every request sent an empty
    /// `X-Goog-Visitor-Id`, which is not treated as anonymous: the player and
    /// metadata endpoints answer `LOGIN_REQUIRED`/"Sign in to confirm you're not
    /// a bot" instead of data. Both halves are asserted so the fallback cannot
    /// silently change where a *present* cookie sends the visitor id.
    #[test]
    fn an_empty_yec_cookie_falls_through_to_the_page_visitor_data() {
        const FROM_HTML: &str = "CgsxMjM0NQ%3D%3D";
        let html =
            format!(r#"<script>window["ytcfg"].set({{"visitorData":"{FROM_HTML}"}});</script>"#);

        assert_eq!(
            resolve_visitor(Some("yec-from-cookie".to_owned()), &html).as_deref(),
            Some("yec-from-cookie"),
            "a non-empty YEC must keep winning"
        );
        assert_eq!(
            resolve_visitor(Some(String::new()), &html).as_deref(),
            Some(FROM_HTML),
            "an empty YEC must fall through to the page's visitorData"
        );
        assert_eq!(
            resolve_visitor(None, &html).as_deref(),
            Some(FROM_HTML),
            "a missing YEC uses the same fallback"
        );
        assert_eq!(
            resolve_visitor(Some(String::new()), "<html></html>"),
            None,
            "no usable id anywhere is the existing Bootstrap error, never an empty id"
        );
    }
}
